#!/usr/bin/env node

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import {
  computeMetadataSelection,
  unavailableSelection,
} from "./ci-metadata-selection.mjs";

const repoRoot = "/workspace";

function packageId(name, root) {
  return `path+file://${repoRoot}/${root}#${name}@0.1.0`;
}

function metadataFixture(records, edges = {}) {
  const roots = new Map(records.map(({ name, root }) => [name, root]));
  const ids = new Map(records.map(({ name, root }) => [name, packageId(name, root)]));
  return {
    packages: records.map(({ name, root }) => ({
      name,
      id: ids.get(name),
      manifest_path: `${repoRoot}/${root}/Cargo.toml`,
      dependencies: (edges[name] ?? []).map((dependency) => ({
        name: dependency,
        path: `${repoRoot}/${roots.get(dependency)}`,
      })),
    })),
    workspace_members: records.map(({ name, root }) => packageId(name, root)),
    resolve: {
      nodes: records.map(({ name }) => ({
        id: ids.get(name),
        deps: (edges[name] ?? []).map((dependency) => ({ pkg: ids.get(dependency) })),
      })),
    },
  };
}

const records = [
  { name: "types", root: "crates/types" },
  { name: "host", root: "crates/host" },
  { name: "mom", root: "products/mom/src-tauri" },
  { name: "unrelated", root: "crates/unrelated" },
];

const metadata = metadataFixture(records, {
  host: ["types"],
  mom: ["host"],
});

const packageGroups = {
  primary: {
    types: ["types"],
    host: ["host"],
    product: ["mom"],
    unrelated: ["unrelated"],
  },
  secondary: {},
};

const pathExceptions = {
  schema: "native-platform.ci-path-exceptions.v2",
  rules: [
    {
      prefix: "products/mom/ui",
      kind: "asset",
      primary_groups: ["product"],
      effects: ["frontend_mom"],
      evidence: "fixture asset",
    },
    {
      prefix: ".github/workflows",
      kind: "workflow",
      effects: ["ignored_tests"],
      evidence: "fixture workflow",
    },
    {
      path: "scripts/release-macos.sh",
      kind: "platform",
      effects: ["platform_macos"],
      evidence: "fixture platform helper",
    },
    {
      prefix: "docs",
      kind: "documentation",
      effects: [],
      evidence: "fixture documentation policy",
    },
  ],
};

function selection(changed, metadataOverride = metadata) {
  return computeMetadataSelection({
    metadata: metadataOverride,
    repoRoot,
    changed,
    packageGroups,
    pathExceptions,
  });
}

test("Cargo metadata supplies changed packages and the complete local reverse closure", () => {
  const result = selection(["crates/types/src/lib.rs"]);
  assert.deepEqual(result.changed_packages, ["types"]);
  assert.deepEqual(result.reverse_dependency_closure, ["host", "mom", "types"]);
  assert.deepEqual(result.primary_groups, ["host", "product", "types"]);
  assert.equal(result.metadata_status, "available");
  assert.equal(result.selection_applied, true);
  assert.equal(result.fallback, "none");
});

test("a reverse-edge mutation deterministically changes the generated closure", () => {
  const withoutHostEdge = metadataFixture(records, { mom: ["host"] });
  assert.deepEqual(selection(["crates/types/src/lib.rs"]).reverse_dependency_closure, [
    "host",
    "mom",
    "types",
  ]);
  assert.deepEqual(
    selection(["crates/types/src/lib.rs"], withoutHostEdge).reverse_dependency_closure,
    ["types"],
  );
});

test("resolved and declared local edges are conservatively unioned", () => {
  const declaredOnly = structuredClone(metadata);
  declaredOnly.resolve.nodes.find((node) => node.id.includes("host@" )).deps = [];
  const result = selection(["crates/types/src/lib.rs"], declaredOnly);
  assert.deepEqual(result.reverse_dependency_closure, ["host", "mom", "types"]);
});

test("explicit non-graph asset, workflow, platform, and documentation classes stay explicit", () => {
  const result = selection([
    ".github/workflows/ci-pr.yml",
    "docs/architecture.md",
    "products/mom/ui/app.js",
    "scripts/release-macos.sh",
  ]);
  assert.deepEqual(
    result.file_classifications.map((record) => record.class),
    ["workflow", "documentation", "asset", "platform"],
  );
  assert.deepEqual(result.primary_groups, ["product"]);
  assert.deepEqual(result.effects, ["frontend_mom", "ignored_tests", "platform_macos"]);
});

test("unknown additions and deletions fail closed to the complete workspace", () => {
  const result = selection(["unknown/input.bin"]);
  assert.equal(result.fallback, "full");
  assert.equal(result.unknown_path_fallback, "full");
  assert.deepEqual(result.unknown_paths, ["unknown/input.bin"]);
  assert.deepEqual(result.reverse_dependency_closure, ["host", "mom", "types", "unrelated"]);
  assert.deepEqual(result.primary_groups, ["host", "product", "types", "unrelated"]);
});

test("metadata unavailability is an unconditional full-selection result", () => {
  const result = unavailableSelection("fixture metadata failure", packageGroups);
  assert.equal(result.metadata_status, "unavailable");
  assert.equal(result.fallback, "full");
  assert.deepEqual(result.fallback_reasons, ["metadata_unavailable"]);
  assert.deepEqual(result.primary_groups, ["host", "product", "types", "unrelated"]);
});

test("missing resolve evidence fails instead of silently dropping reverse edges", () => {
  const incomplete = structuredClone(metadata);
  delete incomplete.resolve;
  assert.throws(
    () => selection(["crates/types/src/lib.rs"], incomplete),
    /resolve\.nodes must be an array/,
  );
});

test("every checked-in workspace package class is accepted by deterministic metadata", () => {
  const root = path.resolve(import.meta.dirname, "../..");
  const checkedInGroups = JSON.parse(
    fs.readFileSync(path.join(root, "ci/package-groups.json"), "utf8"),
  );
  const syntheticRecords = Object.entries(checkedInGroups.primary).flatMap(
    ([group, packages]) =>
      packages.map((name) => ({ name, root: `fixture/${group}/${name}` })),
  );
  const syntheticMetadata = metadataFixture(syntheticRecords);
  const changed = syntheticRecords.map(({ root: packageRoot }) => `${packageRoot}/src/lib.rs`);
  const result = computeMetadataSelection({
    metadata: syntheticMetadata,
    repoRoot,
    changed,
    packageGroups: checkedInGroups,
    pathExceptions: {
      schema: "native-platform.ci-path-exceptions.v2",
      rules: [],
    },
  });
  assert.deepEqual(result.changed_packages, syntheticRecords.map(({ name }) => name).sort());
  assert.deepEqual(result.primary_groups, Object.keys(checkedInGroups.primary).sort());
  assert.equal(result.unknown_paths.length, 0);
});

test("checked-in exceptions are narrow, evidenced, deterministic, and group-valid", () => {
  const root = path.resolve(import.meta.dirname, "../..");
  const checkedInGroups = JSON.parse(
    fs.readFileSync(path.join(root, "ci/package-groups.json"), "utf8"),
  );
  const checkedInExceptions = JSON.parse(
    fs.readFileSync(path.join(root, "ci/ci-path-exceptions.json"), "utf8"),
  );
  assert.equal(checkedInExceptions.schema, "native-platform.ci-path-exceptions.v2");
  const matches = checkedInExceptions.rules.map((entry) => entry.path ?? entry.prefix);
  assert.equal(new Set(matches).size, matches.length);
  for (const entry of checkedInExceptions.rules) {
    assert.notEqual(Boolean(entry.path), Boolean(entry.prefix));
    assert.ok(entry.kind);
    assert.ok(entry.evidence);
    assert.ok(Array.isArray(entry.effects));
    for (const group of entry.primary_groups ?? []) {
      assert.ok(Object.hasOwn(checkedInGroups.primary, group), `${group} is not primary`);
    }
    assert.notEqual(entry.prefix, "products/fte");
    assert.notEqual(entry.prefix, "products/mom");
    assert.notEqual(entry.prefix, "products/loom");
  }
});

// A product library can embed assets outside its Cargo root. Feed that owner
// into the existing graph; do not maintain a parallel list of shell consumers.
function ownedAssetSelection({ changed = ["products/mom/ui/app.js"], edges = { unrelated: ["mom"] }, rules, inputMetadata } = {}) {
  const owned = structuredClone(pathExceptions);
  owned.rules[0].owner_packages = ["mom"];
  return computeMetadataSelection({
    metadata: inputMetadata ?? metadataFixture(records, { host: ["types"], mom: ["host"], ...edges }),
    repoRoot, changed, packageGroups, pathExceptions: rules ?? owned,
  });
}

test("non-Cargo assets seed their owner and its metadata-derived consumers", () => {
  const result = ownedAssetSelection();
  assert.deepEqual(result.changed_packages, []);
  assert.deepEqual(result.asset_owner_packages, ["mom"]);
  assert.deepEqual(result.reverse_dependency_closure, ["mom", "unrelated"]);
  assert.deepEqual(result.primary_groups, ["product", "unrelated"]);
  assert.deepEqual(result.file_classifications[0].owner_packages, ["mom"]);
  assert.deepEqual(result.effects, ["frontend_mom"]);
  assert.equal(result.fallback, "none");
});

test("asset consumers follow edge changes instead of a fixed cross-product allowlist", () => {
  assert.deepEqual(ownedAssetSelection({ edges: {} }).reverse_dependency_closure, ["mom"]);
  const transitive = ownedAssetSelection({ edges: { unrelated: ["mom"], host: ["unrelated"] } });
  assert.deepEqual(transitive.reverse_dependency_closure, ["host", "mom", "unrelated"]);
  assert.ok(!transitive.reverse_dependency_closure.includes("types"), "do not walk forward into dependencies");
});

test("asset owners retain optional and target-specific declared consumers", () => {
  const declaredOnly = metadataFixture(records, { unrelated: ["mom"] });
  for (const node of declaredOnly.resolve.nodes) node.deps = [];
  assert.deepEqual(ownedAssetSelection({ inputMetadata: declaredOnly }).reverse_dependency_closure, ["mom", "unrelated"]);
});

test("asset and Rust seeds are unioned deterministically without relabeling source changes", () => {
  const result = ownedAssetSelection({ changed: ["products/mom/ui/z.js", "crates/types/src/lib.rs", "products/mom/ui/a.js"] });
  assert.deepEqual(result.changed_packages, ["types"]);
  assert.deepEqual(result.asset_owner_packages, ["mom"]);
  assert.deepEqual(result.reverse_dependency_closure, ["host", "mom", "types", "unrelated"]);
  const reversed = ownedAssetSelection({ changed: ["products/mom/ui/a.js", "crates/types/src/lib.rs", "products/mom/ui/z.js"] });
  assert.deepEqual(result.reverse_dependency_closure, reversed.reverse_dependency_closure);
  assert.deepEqual(result.asset_owner_packages, reversed.asset_owner_packages);
});

test("unannotated assets and documentation retain their previous selection boundaries", () => {
  const asset = selection(["products/mom/ui/app.js"]);
  assert.deepEqual(asset.asset_owner_packages, []);
  assert.deepEqual(asset.reverse_dependency_closure, []);
  assert.deepEqual(asset.primary_groups, ["product"]);
  const docs = ownedAssetSelection({ changed: ["docs/guide.md"] });
  assert.deepEqual(docs.asset_owner_packages, []);
  assert.deepEqual(docs.reverse_dependency_closure, []);
  assert.deepEqual(docs.primary_groups, []);
  assert.equal(docs.fallback, "none");
});

test("stale or malformed asset ownership fails instead of omitting a consumer", () => {
  for (const owners of [["removed-package"], [null], [17], "mom", {}, null, true]) {
    const invalid = structuredClone(pathExceptions);
    invalid.rules[0].owner_packages = owners;
    assert.throws(() => ownedAssetSelection({ rules: invalid }), /owner packages.*must be an array|unknown workspace owner/);
  }
  const invalid = structuredClone(pathExceptions);
  invalid.rules[3].owner_packages = ["mom"];
  assert.throws(() => ownedAssetSelection({ rules: invalid }), /only asset exceptions/);
  const fallback = unavailableSelection("stale asset owner", packageGroups);
  assert.equal(fallback.fallback, "full");
  assert.deepEqual(fallback.asset_owner_packages, []);
  assert.deepEqual(fallback.reverse_dependency_closure, ["host", "mom", "types", "unrelated"]);
});

test("asset owners are exact workspace names, not every package in a primary group", () => {
  const groups = structuredClone(packageGroups);
  groups.primary.product.push(...groups.primary.unrelated);
  delete groups.primary.unrelated;
  const rules = structuredClone(pathExceptions);
  rules.rules[0].owner_packages = ["mom", "mom"];
  const result = computeMetadataSelection({ metadata, repoRoot, changed: ["products/mom/ui/app.js"], packageGroups: groups, pathExceptions: rules });
  assert.deepEqual(result.asset_owner_packages, ["mom"]);
  assert.deepEqual(result.reverse_dependency_closure, ["mom"]);
});

test("checked-in Mom asset rules select the linked Loom shell without selecting the whole workspace", () => {
  const root = path.resolve(import.meta.dirname, "../..");
  const groups = JSON.parse(fs.readFileSync(path.join(root, "ci/package-groups.json"), "utf8"));
  const exceptions = JSON.parse(fs.readFileSync(path.join(root, "ci/ci-path-exceptions.json"), "utf8"));
  const syntheticRecords = Object.entries(groups.primary).flatMap(([group, names]) =>
    names.map(name => ({ name, root: `fixture/${group}/${name}` })));
  const linked = metadataFixture(syntheticRecords, { "loom-app": ["mom-llama-app"] });
  for (const changedPath of [
    "products/mom/apps/mom-llama/ui/coop-hx.js",
    "products/mom/apps/mom-llama/package.json",
    "products/mom/contracts/commands.json",
  ]) {
    const result = computeMetadataSelection({ metadata: linked, repoRoot, changed: [changedPath], packageGroups: groups, pathExceptions: exceptions });
    assert.deepEqual(result.asset_owner_packages, ["mom-llama-app"], changedPath);
    assert.deepEqual(result.reverse_dependency_closure, ["loom-app", "mom-llama-app"], changedPath);
    assert.deepEqual(result.primary_groups, ["product-loom", "product-mom"], changedPath);
    assert.equal(result.fallback, "none", changedPath);
    assert.ok(groups.secondary["platform-macos"].some(name => result.reverse_dependency_closure.includes(name)));
    assert.ok(groups.secondary.frontend.includes("loom-app"));
  }
});

// Execute the unchanged production planner over a disposable Git repository.
// Only Cargo metadata and the ignored-registry header are fixtures; no builds,
// credentials, model files, user repository, or hosted services are touched.
function plannedAssetChange(t, changedPath, staleOwner = false) {
  const sourceRoot = path.resolve(import.meta.dirname, "../..");
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "owned-asset-plan-"));
  t.after(() => fs.rmSync(directory, { recursive: true, force: true }));
  const write = (relative, text) => {
    const destination = path.join(directory, relative);
    fs.mkdirSync(path.dirname(destination), { recursive: true });
    fs.writeFileSync(destination, text);
  };
  for (const relative of ["scripts/ci/ci-plan.mjs", "scripts/ci/ci-metadata-selection.mjs", "ci/package-groups.json"]) {
    write(relative, fs.readFileSync(path.join(sourceRoot, relative)));
  }
  const exceptions = JSON.parse(fs.readFileSync(path.join(sourceRoot, "ci/ci-path-exceptions.json"), "utf8"));
  if (staleOwner) exceptions.rules.find(rule => rule.prefix === "products/mom/apps/mom-llama/ui").owner_packages = ["removed-shell"];
  write("ci/ci-path-exceptions.json", JSON.stringify(exceptions));
  write("ci/ignored-tests.json", JSON.stringify({ schema: "native-platform.ignored-tests.v2" }));
  write("products/mom/crates/mom-llama-runtime/Cargo.toml", "# presence fixture\n");
  write("products/loom/apps/loom/src-tauri/Cargo.toml", "# presence fixture\n");
  const groups = JSON.parse(fs.readFileSync(path.join(sourceRoot, "ci/package-groups.json"), "utf8"));
  const syntheticRecords = Object.entries(groups.primary).flatMap(([group, names]) =>
    names.map(name => ({ name, root: `fixture/${group}/${name}` })));
  const metadata = metadataFixture(syntheticRecords, { "loom-app": ["mom-llama-app"] });
  // macOS may create the fixture under /var while process.cwd() resolves it
  // through /private/var. Cargo manifest paths and the planner must agree.
  const portableRoot = fs.realpathSync(directory).split(path.sep).join("/");
  write("metadata.json", JSON.stringify(metadata).replaceAll(repoRoot, portableRoot));
  write("empty-git-config", "");
  const env = {
    PATH: process.env.PATH,
    ...(process.env.SystemRoot ? { SystemRoot: process.env.SystemRoot } : {}),
    HOME: directory, XDG_CONFIG_HOME: directory, GIT_CONFIG_NOSYSTEM: "1",
    GIT_CONFIG_GLOBAL: path.join(directory, "empty-git-config"),
    GIT_AUTHOR_NAME: "CI fixture", GIT_AUTHOR_EMAIL: "ci@example.invalid",
    GIT_COMMITTER_NAME: "CI fixture", GIT_COMMITTER_EMAIL: "ci@example.invalid",
  };
  const git = (...args) => execFileSync("git", args, { cwd: directory, env, encoding: "utf8", timeout: 10_000 }).trim();
  git("init", "--quiet");
  git("add", ".");
  git("commit", "--quiet", "-m", "fixture base");
  const base = git("rev-parse", "HEAD");
  write(changedPath, "changed asset fixture\n");
  git("add", ".");
  git("commit", "--quiet", "-m", "fixture change");
  const head = git("rev-parse", "HEAD");
  return JSON.parse(execFileSync(process.execPath, ["scripts/ci/ci-plan.mjs"], {
    cwd: directory, encoding: "utf8", timeout: 10_000,
    env: { ...env, CI_BASE_SHA: base, CI_HEAD_SHA: head, CI_CARGO_METADATA_PATH: path.join(directory, "metadata.json"), GITHUB_EVENT_NAME: "pull_request" },
  }));
}

test("production planner selects both macOS shells for an owned Mom UI change", (t) => {
  const plan = plannedAssetChange(t, "products/mom/apps/mom-llama/ui/coop-hx.js");
  assert.equal(plan.dependency_selection.metadata_status, "available");
  for (const flag of ["mom", "loom", "frontend_mom", "frontend_loom", "platform_macos"]) assert.equal(plan.flags[flag], true, flag);
  for (const flag of ["full", "native", "gateway", "fuzz"]) assert.equal(plan.flags[flag], false, flag);
  assert.deepEqual(plan.macos_matrix, ["release", "mom", "loom"]);
  assert.deepEqual(plan.dependency_selection.reverse_dependency_closure, ["loom-app", "mom-llama-app"]);
});

test("production planner fails full on stale asset ownership", (t) => {
  const plan = plannedAssetChange(t, "products/mom/apps/mom-llama/ui/coop-hx.js", true);
  assert.equal(plan.dependency_selection.metadata_status, "unavailable");
  assert.match(plan.dependency_selection.reason, /unknown workspace owner: removed-shell/);
  for (const flag of ["full", "root", "mom", "loom", "platform_macos", "frontend_mom", "frontend_loom"]) assert.equal(plan.flags[flag], true, flag);
});

test("production planner keeps documentation-only changes policy-only", (t) => {
  const plan = plannedAssetChange(t, "docs/example.md");
  assert.equal(plan.dependency_selection.metadata_status, "available");
  assert.deepEqual(plan.jobs, ["policy"]);
  assert.deepEqual(plan.macos_matrix, []);
  assert.equal(plan.flags.full, false);
});
