#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

export const REGISTRY_SCHEMA = "native-platform.ignored-tests.v1";
export const CARGO_LIST_ARGUMENTS = [
  "test",
  "--locked",
  "--workspace",
  "--all-targets",
  "--",
  "--ignored",
  "--list",
];

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function sorted(values) {
  return [...values].sort((left, right) => left.localeCompare(right));
}

function duplicates(values) {
  const seen = new Set();
  const duplicate = new Set();
  for (const value of values) {
    if (seen.has(value)) duplicate.add(value);
    seen.add(value);
  }
  return sorted(duplicate);
}

function counts(values) {
  const result = new Map();
  for (const value of values) result.set(value, (result.get(value) ?? 0) + 1);
  return result;
}

function workspacePackageRoots(metadata) {
  const members = new Set(metadata.workspace_members ?? []);
  return new Map(
    (metadata.packages ?? [])
      .filter((candidate) => members.has(candidate.id))
      .map((candidate) => [candidate.name, path.dirname(candidate.manifest_path)]),
  );
}

function ignoredTestsInSource(repoRoot, packageRoots) {
  const rustFiles = new Set();
  const visit = (directory) => {
    for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
      if ([".git", "node_modules", "target"].includes(entry.name)) continue;
      const candidate = path.join(directory, entry.name);
      if (entry.isDirectory()) visit(candidate);
      if (entry.isFile() && entry.name.endsWith(".rs")) rustFiles.add(candidate);
    }
  };
  for (const packageRoot of new Set(packageRoots.values())) visit(packageRoot);

  const results = [];
  const pattern = /#\[ignore(?:\s*=\s*"[^"]*")?\]\s*(?:#\[[^\]]+\]\s*)*(?:async\s+)?fn\s+([A-Za-z0-9_]+)/g;
  for (const source of rustFiles) {
    const sourceText = fs.readFileSync(source, "utf8");
    for (const match of sourceText.matchAll(pattern)) {
      results.push({
        source: path.relative(repoRoot, source).split(path.sep).join("/"),
        function_name: match[1],
      });
    }
  }
  return results.sort((left, right) =>
    `${left.source}:${left.function_name}`.localeCompare(`${right.source}:${right.function_name}`),
  );
}

export function validateRegistry({ registry, metadata, repoRoot }) {
  assert(registry.schema === REGISTRY_SCHEMA, `unexpected ignored-test schema: ${registry.schema}`);
  assert(Number.isInteger(registry.expected_test_count), "expected_test_count must be an integer");
  assert(Array.isArray(registry.entries), "ignored-test entries must be an array");
  assert(
    registry.entries.length === registry.expected_test_count,
    `registry count ${registry.entries.length} != expected ${registry.expected_test_count}`,
  );

  const packageRoots = workspacePackageRoots(metadata);
  const entryKeys = registry.entries.map(
    (entry) => `${entry.package}:${entry.target}:${entry.test_id}`,
  );
  assert(
    duplicates(entryKeys).length === 0,
    `duplicate ignored registry entries: ${duplicates(entryKeys).join(", ")}`,
  );

  for (const entry of registry.entries) {
    for (const field of [
      "test_id",
      "package",
      "target",
      "source",
      "prerequisite",
      "evidence_class",
      "promotion_prohibition",
    ]) {
      assert(typeof entry[field] === "string" && entry[field].trim(), `${entry.test_id}: missing ${field}`);
    }
    assert(Array.isArray(entry.required_environment), `${entry.test_id}: required_environment must be an array`);
    for (const variable of entry.required_environment) {
      assert(/^[A-Z][A-Z0-9_]*$/.test(variable), `${entry.test_id}: invalid environment variable ${variable}`);
    }
    assert(
      /cannot promote/i.test(entry.promotion_prohibition),
      `${entry.test_id}: promotion prohibition must explicitly say what cannot promote`,
    );

    const packageRoot = packageRoots.get(entry.package);
    assert(packageRoot, `${entry.test_id}: unknown workspace package ${entry.package}`);
    const source = path.resolve(repoRoot, entry.source);
    const relativeToPackage = path.relative(packageRoot, source);
    assert(
      relativeToPackage !== ".." && !relativeToPackage.startsWith(`..${path.sep}`),
      `${entry.test_id}: source is outside package ${entry.package}`,
    );
    assert(fs.existsSync(source), `${entry.test_id}: missing source ${entry.source}`);
    const sourceText = fs.readFileSync(source, "utf8");
    const functionName = entry.test_id.split("::").at(-1);
    assert(sourceText.includes("#[ignore"), `${entry.test_id}: source has no ignored test`);
    assert(
      new RegExp(`\\bfn\\s+${functionName}\\b`).test(sourceText),
      `${entry.test_id}: source does not contain the named function`,
    );
  }

  const sourceIgnored = ignoredTestsInSource(repoRoot, packageRoots);
  const registeredSourceKeys = sorted(
    registry.entries.map((entry) => `${entry.source}:${entry.test_id.split("::").at(-1)}`),
  );
  const actualSourceKeys = sourceIgnored.map(
    (entry) => `${entry.source}:${entry.function_name}`,
  );
  assert(
    JSON.stringify(actualSourceKeys) === JSON.stringify(registeredSourceKeys),
    [
      "ignored source registry drift",
      `unregistered: ${actualSourceKeys.filter((key) => !registeredSourceKeys.includes(key)).join(", ")}`,
      `stale: ${registeredSourceKeys.filter((key) => !actualSourceKeys.includes(key)).join(", ")}`,
    ].join("; "),
  );

  return {
    schema: REGISTRY_SCHEMA,
    registry_count: registry.entries.length,
    source_ignored_count: sourceIgnored.length,
    packages: sorted(new Set(registry.entries.map((entry) => entry.package))),
    evidence_classes: sorted(new Set(registry.entries.map((entry) => entry.evidence_class))),
  };
}

export function parseCargoIgnoredList(stdout) {
  return stdout
    .split(/\r?\n/)
    .map((line) => line.match(/^(.+): test$/)?.[1])
    .filter(Boolean);
}

export function reconcileCargoList(registry, actualIds, platform = process.platform) {
  const registryIds = registry.entries.map((entry) => entry.test_id);
  const registeredCounts = counts(registryIds);
  const actualCounts = counts(actualIds);
  const unknown = [];
  for (const [id, count] of actualCounts) {
    for (let index = registeredCounts.get(id) ?? 0; index < count; index += 1) {
      unknown.push(id);
    }
  }
  unknown.sort((left, right) => left.localeCompare(right));
  assert(unknown.length === 0, `Cargo listed unregistered ignored tests: ${unknown.join(", ")}`);

  const missing = [];
  for (const [id, count] of registeredCounts) {
    for (let index = actualCounts.get(id) ?? 0; index < count; index += 1) {
      missing.push(id);
    }
  }
  missing.sort((left, right) => left.localeCompare(right));
  if (platform === "darwin") {
    assert(
      actualIds.length === registry.expected_test_count,
      `Cargo ignored count ${actualIds.length} != registry count ${registry.expected_test_count}`,
    );
    assert(missing.length === 0, `Registry IDs absent from Cargo list: ${missing.join(", ")}`);
  }
  return {
    cargo_count: actualIds.length,
    registry_count: registry.expected_test_count,
    exact_registry_match: missing.length === 0 && actualIds.length === registry.expected_test_count,
    platform,
    missing_platform_filtered_ids: platform === "darwin" ? [] : missing,
  };
}

export function readMetadata(repoRoot) {
  const result = spawnSync(
    process.env.CARGO ?? "cargo",
    ["metadata", "--locked", "--no-deps", "--format-version", "1"],
    { cwd: repoRoot, encoding: "utf8", maxBuffer: 64 * 1024 * 1024 },
  );
  assert(result.status === 0, result.stderr || "cargo metadata failed");
  return JSON.parse(result.stdout);
}

function main() {
  const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
  const registry = JSON.parse(fs.readFileSync(path.join(repoRoot, "ci/ignored-tests.json"), "utf8"));
  const report = validateRegistry({ registry, metadata: readMetadata(repoRoot), repoRoot });

  if (process.argv.includes("--cargo-list")) {
    const result = spawnSync(process.env.CARGO ?? "cargo", CARGO_LIST_ARGUMENTS, {
      cwd: repoRoot,
      encoding: "utf8",
      maxBuffer: 64 * 1024 * 1024,
    });
    assert(result.status === 0, result.stderr || "cargo ignored-list command failed");
    report.cargo_reconciliation = reconcileCargoList(
      registry,
      parseCargoIgnoredList(result.stdout),
    );
  }

  console.log(JSON.stringify(report, null, 2));
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main();
}
