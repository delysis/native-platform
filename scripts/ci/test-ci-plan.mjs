#!/usr/bin/env node

import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

const planner = path.resolve(import.meta.dirname, "ci-plan.mjs");

const metadataPackages = [
  ["llama-native-types", "crates/native/crates/llama-native-types", []],
  ["llama-native-engine", "crates/native/crates/llama-native-engine", ["llama-native-types"]],
  ["llama-native-host", "crates/native/crates/llama-native-host", ["llama-native-engine"]],
  ["attachment-native-types", "crates/services/attachment/crates/attachment-native-types", []],
  ["attachment-native-inspect", "crates/services/attachment/crates/attachment-native-inspect", ["attachment-native-types"]],
  ["information-native-types", "crates/services/information/crates/information-native-types", []],
  ["information-native-store", "crates/services/information/crates/information-native-store", ["information-native-types"]],
  ["speech-native-types", "crates/services/speech/crates/speech-native-types", []],
  ["speech-native-host", "crates/services/speech/crates/speech-native-host", ["speech-native-types"]],
  ["speech-native-platform", "crates/services/speech/crates/speech-native-platform", ["speech-native-types"]],
  ["fte-types", "products/fte/crates/fte-types", []],
  ["fte-backend-llama", "products/fte/crates/fte-backend-llama", ["fte-types", "llama-native-host"]],
  ["free-token-energy", "products/fte/src-tauri", ["fte-backend-llama"]],
  ["mom-llama-runtime", "products/mom/crates/mom-llama-runtime", ["attachment-native-types", "fte-types", "llama-native-host"]],
  ["mom-llama-cli", "products/mom/crates/mom-llama-cli", ["mom-llama-runtime"]],
  ["mom-llama-app", "products/mom/apps/mom-llama/src-tauri", ["mom-llama-runtime", "speech-native-host", "speech-native-platform"]],
  ["loom-types", "products/loom/crates/loom-types", []],
  ["loom-backend-llama", "products/loom/crates/loom-backend-llama", ["llama-native-host", "loom-types"]],
  ["loom-app", "products/loom/apps/loom/src-tauri", ["loom-backend-llama"]],
  ["xtask", "xtask", []],
];

function writeMetadataFixture(repo) {
  const canonicalRepo = fs.realpathSync(repo);
  const ids = new Map(
    metadataPackages.map(([name, packageRoot]) => [
      name,
      `path+file://${canonicalRepo}/${packageRoot}#${name}@0.0.0`,
    ]),
  );
  const roots = new Map(metadataPackages.map(([name, packageRoot]) => [name, packageRoot]));
  const metadata = {
    packages: metadataPackages.map(([name, packageRoot, dependencies]) => ({
      name,
      id: ids.get(name),
      manifest_path: path.join(canonicalRepo, packageRoot, "Cargo.toml"),
      dependencies: dependencies.map((dependency) => ({
        name: dependency,
        path: path.join(canonicalRepo, roots.get(dependency)),
      })),
    })),
    workspace_members: metadataPackages.map(([name]) => ids.get(name)),
    resolve: {
      nodes: metadataPackages.map(([name, , dependencies]) => ({
        id: ids.get(name),
        deps: dependencies.map((dependency) => ({ pkg: ids.get(dependency) })),
      })),
    },
  };
  const metadataPath = path.join(repo, ".ci-cargo-metadata.json");
  fs.writeFileSync(metadataPath, `${JSON.stringify(metadata)}\n`);
  return metadataPath;
}

function git(cwd, ...args) {
  return execFileSync("git", args, { cwd, encoding: "utf8" }).trim();
}

function write(repo, relativePath, contents = "fixture\n") {
  const absolutePath = path.join(repo, relativePath);
  fs.mkdirSync(path.dirname(absolutePath), { recursive: true });
  fs.writeFileSync(absolutePath, contents);
}

function commit(repo, message) {
  git(repo, "add", "--all");
  git(repo, "commit", "-qm", message);
  return git(repo, "rev-parse", "HEAD");
}

function makeRepo() {
  const repo = fs.mkdtempSync(path.join(os.tmpdir(), "native-platform-ci-plan-"));
  git(repo, "init", "-q");
  git(repo, "config", "user.email", "ci-plan@example.invalid");
  git(repo, "config", "user.name", "CI planner tests");
  write(repo, "README.md", "base\n");
  const base = commit(repo, "base");
  return { repo, base };
}

function plan(repo, base, head, outputPath, metadataPath = writeMetadataFixture(repo)) {
  const result = spawnSync(process.execPath, [planner], {
    cwd: repo,
    encoding: "utf8",
    env: {
      ...process.env,
      CI_BASE_SHA: base,
      CI_HEAD_SHA: head,
      GITHUB_EVENT_NAME: "pull_request",
      CI_CARGO_METADATA_PATH: metadataPath,
      CARGO: "/fixture/cargo-must-not-be-probed",
      ...(outputPath ? { GITHUB_OUTPUT: outputPath } : {}),
    },
  });
  assert.equal(result.status, 0, result.stderr);
  const parsed = JSON.parse(result.stdout);
  if (metadataPath === path.join(repo, ".ci-cargo-metadata.json")) {
    assert.equal(
      parsed.dependency_selection.metadata_status,
      "available",
      parsed.dependency_selection.reason,
    );
  }
  return parsed;
}

function fixture(relativePath, { contents = "changed\n", present = [] } = {}) {
  const { repo, base } = makeRepo();
  for (const presentPath of present) write(repo, presentPath, "[workspace]\n");
  const fixtureBase = present.length > 0 ? commit(repo, "fixture presence") : base;
  write(repo, relativePath, contents);
  const head = commit(repo, relativePath);
  return { repo, result: plan(repo, fixtureBase, head) };
}

function fixtureMany(relativePaths, { present = [] } = {}) {
  const { repo, base } = makeRepo();
  for (const presentPath of present) write(repo, presentPath, "[workspace]\n");
  const fixtureBase = present.length > 0 ? commit(repo, "fixture presence") : base;
  for (const relativePath of relativePaths) write(repo, relativePath);
  const head = commit(repo, "fixture changes");
  return { repo, result: plan(repo, fixtureBase, head) };
}

test("docs-only changes require policy and nothing else", () => {
  const { result } = fixture("docs/architecture.md");
  assert.equal(result.risk, "docs");
  assert.deepEqual(result.jobs, ["policy"]);
  assert.deepEqual(result.macos_matrix, []);
});

test("CI policy changes use root Linux and macOS without expanding to full", () => {
  const { result } = fixture("scripts/ci/ci-plan.mjs");
  assert.equal(result.risk, "behavior");
  assert.equal(result.flags.root, true);
  assert.equal(result.flags.platform_macos, true);
  assert.equal(result.flags.full, false);
  assert.deepEqual(result.jobs, [
    "policy",
    "root-linux",
    "platform-macos",
    "ignored-tests",
  ]);
  assert.deepEqual(result.macos_matrix, ["release", "root"]);
});

test("macOS release tooling selects only policy and the macOS syntax lane", () => {
  for (const relativePath of [
    "scripts/release-macos.sh",
    "scripts/smoke-macos-app.sh",
    "scripts/find-embedded-model.mjs",
    "scripts/product-state-backup.mjs",
  ]) {
    const { result } = fixture(relativePath);
    assert.equal(result.risk, "release");
    assert.equal(result.flags.root, false);
    assert.equal(result.flags.platform_macos, true);
    assert.equal(result.flags.full, false);
    assert.deepEqual(result.jobs, ["policy", "platform-macos"]);
    assert.deepEqual(result.macos_matrix, ["release"]);
  }
});

test("Native changes require root, Native, and macOS product coverage", () => {
  const { result } = fixture("crates/native/crates/llama-native-engine/src/lib.rs");
  assert.equal(result.risk, "behavior");
  assert.equal(result.flags.root, true);
  assert.equal(result.flags.native, true);
  assert.equal(result.flags.platform_linux, true);
  assert.equal(result.flags.platform_macos, true);
  assert.ok(!("platform_windows" in result.flags));
  assert.ok(result.jobs.includes("native-linux"));
  assert.deepEqual(result.macos_matrix, ["release", "root"]);
});

test("Attachment inspection changes select Attachment and fuzz only", () => {
  const { result } = fixture(
    "crates/services/attachment/crates/attachment-native-inspect/src/lib.rs",
  );
  assert.equal(result.flags.attachment, true);
  assert.equal(result.flags.fuzz, true);
  assert.equal(result.flags.speech, false);
  assert.ok(result.jobs.includes("attachment-linux"));
  assert.ok(result.jobs.includes("fuzz-build"));
});

test("Information changes select Information", () => {
  const { result } = fixture(
    "crates/services/information/crates/information-native-store/src/lib.rs",
  );
  assert.equal(result.flags.information, true);
  assert.equal(result.flags.full, false);
});

test("Speech Apple changes select Speech and platform coverage", () => {
  const { result } = fixture(
    "crates/services/speech/crates/speech-native-platform/src/apple.rs",
  );
  assert.equal(result.flags.speech, true);
  assert.equal(result.flags.platform_macos, true);
  assert.ok(!result.jobs.includes("platform-windows"));
  assert.deepEqual(result.macos_matrix, ["release", "speech"]);
});

test("contract-family changes include metadata consumers while the Mom overlay stays shadow-only", () => {
  const contractPaths = [
    "crates/native/crates/llama-native-types/src/lib.rs",
    "crates/services/attachment/crates/attachment-native-types/src/lib.rs",
    "crates/services/information/crates/information-native-types/src/lib.rs",
    "crates/services/speech/crates/speech-native-types/src/lib.rs",
    "products/fte/crates/fte-types/src/lib.rs",
  ];
  for (const contractPath of contractPaths) {
    const { result } = fixture(contractPath, {
      present: ["products/mom/Cargo.toml"],
    });
    assert.equal(result.flags.mom, true, contractPath);
    assert.ok(result.jobs.includes("mom-linux"), contractPath);
    assert.equal(result.conservative_overlays.mom_contracts.applied, true);
    assert.deepEqual(result.conservative_overlays.mom_contracts.paths, [contractPath]);
    assert.equal(result.conservative_overlays.mom_contracts.applied_to_selection, false);
    assert.equal(result.dependency_selection.selection_applied, true);
    assert.equal(result.dependency_selection.metadata_status, "available");
    assert.equal(result.dependency_shadow.selection_applied, false);
  }
});

test("contract-family documentation does not trigger the temporary Mom overlay", () => {
  const { result } = fixture("crates/services/speech/docs/ARCHITECTURE.md", {
    present: ["products/mom/Cargo.toml"],
  });
  assert.equal(result.flags.speech, true);
  assert.equal(result.flags.mom, false);
  assert.equal(result.conservative_overlays.mom_contracts.applied, false);
});

test("an unexplained legacy reduction forces full instead of narrowing generated selection", () => {
  const { result } = fixture(
    "crates/services/information/crates/information-native-types/src/lib.rs",
    { present: ["products/mom/Cargo.toml"] },
  );
  assert.equal(result.dependency_selection.fallback, "full");
  assert.ok(
    result.dependency_selection.fallback_reasons.includes(
      "legacy_reduction_without_evidence",
    ),
  );
  assert.equal(result.flags.full, true);
  assert.ok(result.dependency_shadow.missing_from_generated.includes("job:mom-linux"));
  assert.ok(result.dependency_shadow.final_surface.includes("flag:full"));
});

test("an explicit non-graph evidence rule can authorize a reviewed legacy reduction", () => {
  const { result } = fixture("products/mom/docs/PRODUCT.md", {
    present: ["products/mom/Cargo.toml"],
  });
  assert.equal(result.flags.full, false);
  assert.deepEqual(result.jobs, ["policy"]);
  assert.deepEqual(result.dependency_shadow.missing_from_generated, ["job:mom-linux"]);
  assert.deepEqual(result.dependency_shadow.reduction_evidence, [
    {
      path: "products/mom/docs/PRODUCT.md",
      rule: "products/mom/docs",
      evidence: "Mom documentation is policy-only",
    },
  ]);
});

test("Mom native source selects its product and macOS parity without root duplication", () => {
  const { result } = fixture("products/mom/apps/mom-llama/src-tauri/src/commands.rs", {
    present: ["products/mom/Cargo.toml"],
  });
  assert.equal(result.presence.mom, true);
  assert.equal(result.flags.mom, true);
  assert.equal(result.flags.root, false);
  assert.equal(result.flags.platform_macos, true);
  assert.deepEqual(result.jobs, [
    "policy",
    "mom-linux",
    "platform-macos",
    "ignored-tests",
  ]);
  assert.deepEqual(result.macos_matrix, ["release", "mom"]);
});

test("the PR 22 Mom diff has the focused product, frontend, and macOS plan", () => {
  const { result } = fixtureMany(
    [
      "products/mom/apps/mom-llama/src-tauri/src/commands.rs",
      "products/mom/apps/mom-llama/src-tauri/src/view.rs",
      "products/mom/apps/mom-llama/ui/coop-hx.js",
      "products/mom/crates/mom-llama-cli/src/main.rs",
      "products/mom/crates/mom-llama-runtime/src/config.rs",
      "products/mom/crates/mom-llama-runtime/src/server.rs",
      "products/mom/crates/mom-llama-runtime/tests/runtime.rs",
    ],
    { present: ["products/mom/Cargo.toml"] },
  );
  assert.equal(result.flags.root, false);
  assert.equal(result.flags.mom, true);
  assert.equal(result.flags.frontend_mom, true);
  assert.equal(result.flags.platform_macos, true);
  assert.deepEqual(result.jobs, [
    "policy",
    "mom-linux",
    "frontend",
    "platform-macos",
    "ignored-tests",
  ]);
});

test("ignored-test sources, targets, and registry changes select authoritative reconciliation", () => {
  for (const relativePath of [
    "ci/ignored-tests.json",
    "crates/services/information/crates/information-native-backend-sqlite/src/lib.rs",
    "products/mom/crates/mom-llama-runtime/Cargo.toml",
  ]) {
    const { result } = fixture(relativePath, {
      present: ["products/mom/Cargo.toml"],
    });
    assert.equal(result.flags.ignored_tests, true, relativePath);
    assert.ok(result.jobs.includes("ignored-tests"), relativePath);
  }
});

test("new and deleted Rust sources select authoritative reconciliation", () => {
  const added = fixture(
    "crates/native/crates/llama-native-types/src/new_ignored.rs",
    { contents: "#[test]\n#[ignore]\nfn newly_ignored() {}\n" },
  ).result;
  assert.equal(added.flags.full, false);
  assert.equal(added.flags.ignored_tests, true);
  assert.ok(added.jobs.includes("ignored-tests"));

  const { repo } = makeRepo();
  const source = "crates/services/attachment/crates/attachment-native-types/src/removed.rs";
  write(repo, source, "#[test]\n#[ignore]\nfn removed_ignored() {}\n");
  const withIgnored = commit(repo, "add ignored test");
  fs.rmSync(path.join(repo, source));
  const withoutIgnored = commit(repo, "remove ignored test");
  const removed = plan(repo, withIgnored, withoutIgnored);
  assert.equal(removed.flags.full, false);
  assert.equal(removed.flags.ignored_tests, true);
  assert.ok(removed.jobs.includes("ignored-tests"));
});

test("all Rust syntax shapes select ignored-test reconciliation fail closed", () => {
  for (const [name, contents] of [
    ["ordinary", "pub fn ordinary() {}\n"],
    ["comment-decoy", "// #[ignore]\npub fn comment_decoy() {}\n"],
    ["cfg-attr", "#[cfg_attr(any(), ignore)]\nfn conditional() {}\n"],
    ["macro", "macro_rules! tests { () => { #[ignore] fn made() {} } }\n"],
    ["public", "#[ignore]\npub fn public_test() {}\n"],
  ]) {
    const { result } = fixture(
      `crates/native/crates/llama-native-types/src/${name}.rs`,
      { contents },
    );
    assert.equal(result.flags.full, false, name);
    assert.equal(result.flags.ignored_tests, true, name);
    assert.ok(result.jobs.includes("ignored-tests"), name);
  }
});

test("Cargo, build-script, proc-macro, and toolchain inputs select reconciliation", () => {
  for (const relativePath of [
    "crates/native/crates/llama-native-engine/Cargo.toml",
    "crates/native/crates/llama-native-engine/build.rs",
    "crates/native/crates/example-proc-macro/src/lib.rs",
    ".cargo/config.toml",
    "Cargo.lock",
    "ci/package-groups.json",
    "rust-toolchain.toml",
  ]) {
    const { result } = fixture(relativePath);
    assert.equal(result.flags.ignored_tests, true, relativePath);
    assert.ok(result.jobs.includes("ignored-tests"), relativePath);
  }
});

test("product package scripts select their owned frontend checks", () => {
  const mom = fixture("products/mom/apps/mom-llama/package.json", {
    contents: '{"scripts":{"check:frontend":"node --check ui/coop-hx.js"}}\n',
    present: ["products/mom/Cargo.toml"],
  }).result;
  assert.equal(mom.flags.frontend_mom, true);
  assert.deepEqual(mom.jobs, ["policy", "mom-linux", "frontend"]);

  const fte = fixture("products/fte/package.json").result;
  assert.equal(fte.flags.frontend_fte, true);
  assert.ok(fte.jobs.includes("frontend"));
});

test("Mom dependency metadata remains conservative", () => {
  const { result } = fixture("products/mom/crates/mom-llama-runtime/Cargo.toml", {
    contents: "[package]\nname = \"mom-llama-runtime\"\n",
    present: ["products/mom/Cargo.toml"],
  });
  assert.equal(result.flags.root, true);
  assert.equal(result.flags.mom, true);
  assert.equal(result.flags.dependency_graph, true);
  assert.equal(result.flags.platform_macos, true);
  assert.deepEqual(result.jobs, [
    "policy",
    "root-linux",
    "mom-linux",
    "platform-macos",
    "ignored-tests",
    "dependency-graph",
  ]);
  assert.deepEqual(result.macos_matrix, ["release", "root", "mom"]);
});

test("Loom Svelte source selects Loom frontend when Loom is present", () => {
  const { result } = fixture("products/loom/apps/loom/src/App.svelte", {
    present: ["products/loom/apps/loom/src-tauri/Cargo.toml"],
  });
  assert.equal(result.presence.loom, true);
  assert.equal(result.flags.loom, true);
  assert.equal(result.flags.frontend_loom, true);
  assert.ok(result.jobs.includes("loom-linux"));
  assert.ok(result.jobs.includes("frontend"));
});

test("root Cargo metadata forces the complete present graph", () => {
  const { result } = fixture("Cargo.toml", {
    contents: "[workspace]\n",
    present: [
      "products/mom/Cargo.toml",
      "products/loom/apps/loom/src-tauri/Cargo.toml",
    ],
  });
  assert.equal(result.risk, "dependency");
  assert.equal(result.flags.full, true);
  assert.equal(result.flags.dependency_graph, true);
  assert.ok(result.jobs.includes("mom-linux"));
  assert.ok(result.jobs.includes("loom-linux"));
  assert.ok(result.jobs.includes("platform-macos"));
  assert.ok(!result.jobs.includes("platform-windows"));
  assert.deepEqual(result.macos_matrix, [
    "release",
    "root",
    "mom",
    "attachment",
    "information",
    "speech",
    "loom",
  ]);
});

test("migration maps retain policy verification without history replay", () => {
  const { result } = fixture("migration/example.commit-map");
  assert.equal(result.risk, "import");
  assert.deepEqual(result.jobs, ["policy"]);
});

test("unknown additions and deletions fail closed to full", () => {
  const added = fixture("unexpected/new-input.bin").result;
  assert.equal(added.flags.full, true);

  const { repo, base } = makeRepo();
  write(repo, "unexpected/old-input.bin");
  const withUnknown = commit(repo, "add unknown");
  fs.rmSync(path.join(repo, "unexpected/old-input.bin"));
  const deleted = commit(repo, "delete unknown");
  assert.equal(plan(repo, withUnknown, deleted).flags.full, true);
  assert.notEqual(base, withUnknown);
});

test("metadata unavailability forces the unchanged complete job and macOS matrices", () => {
  const { repo, base } = makeRepo();
  write(repo, "docs/note.md");
  const head = commit(repo, "docs");
  const result = plan(
    repo,
    base,
    head,
    undefined,
    path.join(repo, "missing-cargo-metadata.json"),
  );
  assert.equal(result.dependency_selection.metadata_status, "unavailable");
  assert.equal(result.dependency_selection.fallback, "full");
  assert.equal(result.flags.full, true);
  assert.deepEqual(result.macos_matrix, [
    "release",
    "root",
    "attachment",
    "information",
    "speech",
  ]);
  assert.deepEqual(result.jobs, [
    "policy",
    "root-linux",
    "native-linux",
    "gateway-linux",
    "attachment-linux",
    "information-linux",
    "speech-linux",
    "frontend",
    "platform-macos",
    "ignored-tests",
    "dependency-graph",
    "fuzz-build",
  ]);
});

test("planner applies metadata reverse consumers and retains a legacy shadow report", () => {
  const { result } = fixture(
    "crates/native/crates/llama-native-types/src/lib.rs",
    {
      present: [
        "products/mom/Cargo.toml",
        "products/loom/apps/loom/src-tauri/Cargo.toml",
      ],
    },
  );
  assert.deepEqual(result.dependency_selection.changed_packages, ["llama-native-types"]);
  for (const packageName of [
    "free-token-energy",
    "fte-backend-llama",
    "llama-native-engine",
    "llama-native-host",
    "loom-app",
    "loom-backend-llama",
    "mom-llama-app",
    "mom-llama-cli",
    "mom-llama-runtime",
  ]) {
    assert.ok(
      result.dependency_selection.reverse_dependency_closure.includes(packageName),
      packageName,
    );
  }
  assert.equal(result.flags.full, false);
  assert.equal(result.flags.gateway, true);
  assert.equal(result.flags.mom, true);
  assert.equal(result.flags.loom, true);
  assert.equal(result.dependency_shadow.mode, "legacy-shadow");
  assert.equal(result.dependency_shadow.generated_is_at_least_as_conservative, true);
});

test("renaming runtime source into docs retains the source-side coverage", () => {
  const { repo } = makeRepo();
  write(repo, "crates/native/crates/llama-native-types/src/old.rs");
  const sourceHead = commit(repo, "add native source");
  fs.mkdirSync(path.join(repo, "docs"), { recursive: true });
  git(
    repo,
    "mv",
    "crates/native/crates/llama-native-types/src/old.rs",
    "docs/old.md",
  );
  const renamedHead = commit(repo, "move source into docs");
  const result = plan(repo, sourceHead, renamedHead);
  assert.equal(result.flags.native, true);
  assert.equal(result.flags.root, true);
});

test("GitHub output carries the compact plan and every declared flag", () => {
  const { repo, base } = makeRepo();
  write(repo, "docs/note.md");
  const head = commit(repo, "docs");
  const outputPath = path.join(repo, "github-output.txt");
  const result = plan(repo, base, head, outputPath);
  const output = fs.readFileSync(outputPath, "utf8");
  assert.match(output, /^plan_json=/m);
  for (const flag of Object.keys(result.flags)) {
    assert.match(output, new RegExp(`^${flag}=(?:true|false)$`, "m"));
  }
  assert.match(output, /^macos_matrix=\[\]$/m);
});
