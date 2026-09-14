#!/usr/bin/env node

import assert from "node:assert/strict";
import fs from "node:fs";
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
