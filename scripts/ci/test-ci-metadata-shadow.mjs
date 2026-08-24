#!/usr/bin/env node

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { computeReverseDependencyShadow } from "./ci-metadata-shadow.mjs";

const repoRoot = "/workspace";

function packageRecord(name, root, dependencies = []) {
  return {
    name,
    id: `path+file://${repoRoot}/${root}#0.1.0`,
    manifest_path: `${repoRoot}/${root}/Cargo.toml`,
    dependencies: dependencies.map((dependencyRoot) => ({
      name: dependencyRoot.split("/").at(-1),
      path: `${repoRoot}/${dependencyRoot}`,
    })),
  };
}

const metadata = {
  packages: [
    packageRecord("types", "crates/types"),
    packageRecord("host", "crates/host", ["crates/types"]),
    packageRecord("mom", "products/mom/src-tauri", ["crates/host"]),
    packageRecord("unrelated", "crates/unrelated"),
  ],
  workspace_members: [
    `path+file://${repoRoot}/crates/types#0.1.0`,
    `path+file://${repoRoot}/crates/host#0.1.0`,
    `path+file://${repoRoot}/products/mom/src-tauri#0.1.0`,
    `path+file://${repoRoot}/crates/unrelated#0.1.0`,
  ],
};

const packageGroups = {
  primary: {
    types: ["types"],
    host: ["host"],
    product: ["mom"],
    unrelated: ["unrelated"],
  },
};

const pathExceptions = {
  asset_groups: [{ prefix: "products/mom/ui", primary_group: "product" }],
  authoritative_exceptions: [{ prefix: "docs", kind: "documentation" }],
};

function shadow(changed, authoritativePrimaryGroups = []) {
  return computeReverseDependencyShadow({
    metadata,
    repoRoot,
    changed,
    packageGroups,
    pathExceptions,
    authoritativePrimaryGroups,
  });
}

test("Cargo metadata supplies the complete local reverse-dependency closure", () => {
  const result = shadow(["crates/types/src/lib.rs"], ["types", "host", "product"]);
  assert.deepEqual(result.changed_packages, ["types"]);
  assert.deepEqual(result.reverse_dependency_closure, ["host", "mom", "types"]);
  assert.deepEqual(result.primary_groups, ["host", "product", "types"]);
  assert.equal(result.matches_authoritative, true);
  assert.equal(result.selection_applied, false);
  assert.equal(result.promotion_allowed, false);
});

test("a shadow mismatch is explicit and cannot alter authoritative selection", () => {
  const result = shadow(["crates/types/src/lib.rs"], ["types"]);
  assert.deepEqual(result.missing_from_authoritative, ["host", "product"]);
  assert.deepEqual(result.extra_in_authoritative, []);
  assert.equal(result.matches_authoritative, false);
  assert.equal(result.selection_applied, false);
  assert.match(result.promotion_prohibition, /observational only/);
});

test("explicit asset rules map non-Cargo product files", () => {
  const result = shadow(["products/mom/ui/app.js"], ["product"]);
  assert.deepEqual(result.changed_packages, []);
  assert.deepEqual(result.primary_groups, ["product"]);
  assert.deepEqual(result.explicit_path_exceptions, [
    {
      path: "products/mom/ui/app.js",
      kind: "asset",
      rule: "products/mom/ui",
    },
  ]);
  assert.equal(result.matches_authoritative, true);
});

test("unknown paths fail closed to the complete metadata graph", () => {
  const result = shadow(["unknown/input.bin"], [
    "host",
    "product",
    "types",
    "unrelated",
  ]);
  assert.equal(result.unknown_path_fallback, "full");
  assert.deepEqual(result.unknown_paths, ["unknown/input.bin"]);
  assert.deepEqual(result.reverse_dependency_closure, [
    "host",
    "mom",
    "types",
    "unrelated",
  ]);
  assert.equal(result.matches_authoritative, true);
});

test("checked-in asset exceptions name existing primary groups and remain explicit", () => {
  const root = path.resolve(import.meta.dirname, "../..");
  const checkedInGroups = JSON.parse(
    fs.readFileSync(path.join(root, "ci/package-groups.json"), "utf8"),
  );
  const checkedInExceptions = JSON.parse(
    fs.readFileSync(path.join(root, "ci/ci-path-exceptions.json"), "utf8"),
  );
  assert.equal(
    checkedInExceptions.schema,
    "native-platform.ci-path-exceptions.v1",
  );
  const prefixes = checkedInExceptions.asset_groups.map((entry) => entry.prefix);
  assert.equal(new Set(prefixes).size, prefixes.length);
  for (const entry of checkedInExceptions.asset_groups) {
    assert.ok(
      Object.hasOwn(checkedInGroups.primary, entry.primary_group),
      `unknown primary group for ${entry.prefix}`,
    );
    assert.notEqual(entry.prefix, "products/fte");
    assert.notEqual(entry.prefix, "products/mom");
    assert.notEqual(entry.prefix, "products/loom");
  }
  for (const entry of checkedInExceptions.authoritative_exceptions) {
    assert.notEqual(Boolean(entry.path), Boolean(entry.prefix));
    assert.ok(entry.kind);
  }
});
