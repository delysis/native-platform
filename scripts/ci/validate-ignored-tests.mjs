#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

export const REGISTRY_SCHEMA = "native-platform.ignored-tests.v2";
export const SUPPORTED_PLATFORMS = ["linux", "macos", "windows"];
export const CARGO_BUILD_ARGUMENTS = [
  "test",
  "--locked",
  "--workspace",
  "--all-targets",
  "--no-run",
  "--message-format=json-render-diagnostics",
];
export const TEST_HARNESS_LIST_ARGUMENTS = ["--ignored", "--list"];

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

function repoRelative(repoRoot, candidate) {
  const absolute = path.isAbsolute(candidate)
    ? candidate
    : path.resolve(repoRoot, candidate);
  return path.relative(repoRoot, absolute).split(path.sep).join("/");
}

function canonicalKinds(kinds) {
  return sorted(kinds);
}

function targetReference(packageName, selector) {
  return `${packageName}:${selector}`;
}

function targetIdentity({ package: packageName, name, kinds, src_path: srcPath }) {
  return {
    package: packageName,
    name,
    kinds: canonicalKinds(kinds),
    src_path: srcPath,
  };
}

function metadataTargetIdentity(packageName, target, repoRoot) {
  return targetIdentity({
    package: packageName,
    name: target.name,
    kinds: target.kind,
    src_path: repoRelative(repoRoot, target.src_path),
  });
}

function targetIdentityKey(target) {
  return JSON.stringify([
    target.package,
    target.name,
    canonicalKinds(target.kinds),
    target.src_path,
  ]);
}

function inventoryKey(entry) {
  return JSON.stringify([targetIdentityKey(entry.target), entry.test_id]);
}

function describeInventory(entry) {
  const kind = canonicalKinds(entry.target.kinds).join("+");
  return `${entry.target.package}:${kind}:${entry.target.name}:${entry.test_id}`;
}

function validatePlatforms(platforms, label) {
  assert(
    Array.isArray(platforms) && platforms.length > 0,
    `${label}: platforms must be a non-empty array`,
  );
  assert(
    duplicates(platforms).length === 0,
    `${label}: duplicate platforms: ${duplicates(platforms).join(", ")}`,
  );
  for (const platform of platforms) {
    assert(
      SUPPORTED_PLATFORMS.includes(platform),
      `${label}: unsupported platform ${platform}`,
    );
  }
  const canonical = SUPPORTED_PLATFORMS.filter((platform) => platforms.includes(platform));
  assert(
    JSON.stringify(platforms) === JSON.stringify(canonical),
    `${label}: platforms must use canonical order ${canonical.join(", ")}`,
  );
}

function workspacePackages(metadata) {
  const members = new Set(metadata.workspace_members ?? []);
  const packages = (metadata.packages ?? []).filter((candidate) => members.has(candidate.id));
  const duplicateNames = duplicates(packages.map((candidate) => candidate.name));
  assert(
    duplicateNames.length === 0,
    `duplicate workspace package names: ${duplicateNames.join(", ")}`,
  );
  return new Map(packages.map((candidate) => [candidate.name, candidate]));
}

function workspacePackageRoots(metadata) {
  return new Map(
    [...workspacePackages(metadata)].map(([name, candidate]) => [
      name,
      path.dirname(candidate.manifest_path),
    ]),
  );
}

function validateCargoTargets(registry, metadata, repoRoot) {
  assert(Array.isArray(registry.cargo_targets), "cargo_targets must be an array");
  const packages = workspacePackages(metadata);
  const references = new Map();
  const identities = [];

  for (const target of registry.cargo_targets) {
    for (const field of ["package", "selector", "name", "src_path", "manifest_path"]) {
      assert(
        typeof target[field] === "string" && target[field].trim(),
        `Cargo target is missing ${field}`,
      );
    }
    assert(
      Array.isArray(target.kinds) && target.kinds.length > 0,
      `${target.package}:${target.selector}: kinds must be a non-empty array`,
    );
    assert(
      target.kinds.every((kind) => typeof kind === "string" && kind.trim()),
      `${target.package}:${target.selector}: kinds must contain strings`,
    );
    assert(
      duplicates(target.kinds).length === 0,
      `${target.package}:${target.selector}: duplicate kinds`,
    );
    assert(
      JSON.stringify(target.kinds) === JSON.stringify(canonicalKinds(target.kinds)),
      `${target.package}:${target.selector}: kinds must be sorted`,
    );
    validatePlatforms(target.platforms, `${target.package}:${target.selector}`);

    const reference = targetReference(target.package, target.selector);
    assert(!references.has(reference), `duplicate Cargo target selector ${reference}`);

    const workspacePackage = packages.get(target.package);
    assert(workspacePackage, `${reference}: unknown workspace package`);
    const actualManifest = repoRelative(repoRoot, workspacePackage.manifest_path);
    assert(
      actualManifest === target.manifest_path,
      `${reference}: manifest ${target.manifest_path} != Cargo metadata ${actualManifest}`,
    );

    const expectedIdentity = targetIdentity(target);
    const matches = workspacePackage.targets.filter(
      (candidate) =>
        targetIdentityKey(metadataTargetIdentity(target.package, candidate, repoRoot)) ===
        targetIdentityKey(expectedIdentity),
    );
    assert(
      matches.length === 1,
      `${reference}: Cargo target not found for name=${target.name}, ` +
        `kinds=${target.kinds.join("+")}, src_path=${target.src_path}`,
    );

    references.set(reference, target);
    identities.push(targetIdentityKey(expectedIdentity));
  }

  const duplicateIdentities = duplicates(identities);
  assert(
    duplicateIdentities.length === 0,
    `Cargo target identities have multiple selectors: ${duplicateIdentities.join(", ")}`,
  );
  return references;
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
  const pattern =
    /#\[ignore(?:\s*=\s*"[^"]*")?\]\s*(?:#\[[^\]]+\]\s*)*(?:async\s+)?fn\s+([A-Za-z0-9_]+)/g;
  for (const source of rustFiles) {
    const sourceText = fs.readFileSync(source, "utf8");
    for (const match of sourceText.matchAll(pattern)) {
      results.push({
        source: repoRelative(repoRoot, source),
        function_name: match[1],
      });
    }
  }
  return results.sort((left, right) =>
    `${left.source}:${left.function_name}`.localeCompare(`${right.source}:${right.function_name}`),
  );
}

export function registryPlatform(nodePlatform = process.platform) {
  const aliases = { darwin: "macos", win32: "windows" };
  const platform = aliases[nodePlatform] ?? nodePlatform;
  assert(
    SUPPORTED_PLATFORMS.includes(platform),
    `unsupported current platform ${nodePlatform}`,
  );
  return platform;
}

export function validateRegistry({ registry, metadata, repoRoot }) {
  assert(
    registry.schema === REGISTRY_SCHEMA,
    `unexpected ignored-test schema: ${registry.schema}`,
  );
  assert(Number.isInteger(registry.expected_test_count), "expected_test_count must be an integer");
  assert(Array.isArray(registry.entries), "ignored-test entries must be an array");
  assert(
    registry.entries.length === registry.expected_test_count,
    `registry count ${registry.entries.length} != expected ${registry.expected_test_count}`,
  );

  const packageRoots = workspacePackageRoots(metadata);
  const cargoTargets = validateCargoTargets(registry, metadata, repoRoot);
  const entryKeys = registry.entries.map(
    (entry) => `${entry.package}:${entry.target}:${entry.test_id}`,
  );
  assert(
    duplicates(entryKeys).length === 0,
    `duplicate ignored registry entries: ${duplicates(entryKeys).join(", ")}`,
  );

  const referencedTargets = new Set();
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
      assert(
        typeof entry[field] === "string" && entry[field].trim(),
        `${entry.test_id}: missing ${field}`,
      );
    }
    validatePlatforms(entry.platforms, entry.test_id);
    assert(
      Array.isArray(entry.required_environment),
      `${entry.test_id}: required_environment must be an array`,
    );
    for (const variable of entry.required_environment) {
      assert(
        /^[A-Z][A-Z0-9_]*$/.test(variable),
        `${entry.test_id}: invalid environment variable ${variable}`,
      );
    }
    assert(
      /cannot promote/i.test(entry.promotion_prohibition),
      `${entry.test_id}: promotion prohibition must explicitly say what cannot promote`,
    );

    const targetKey = targetReference(entry.package, entry.target);
    const cargoTarget = cargoTargets.get(targetKey);
    assert(cargoTarget, `${entry.test_id}: nonexistent Cargo target ${targetKey}`);
    referencedTargets.add(targetKey);
    for (const platform of entry.platforms) {
      assert(
        cargoTarget.platforms.includes(platform),
        `${entry.test_id}: test is available on ${platform}, but Cargo target ${targetKey} is not`,
      );
    }

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

  const unusedTargets = [...cargoTargets.keys()].filter(
    (key) => !referencedTargets.has(key),
  );
  assert(unusedTargets.length === 0, `unreferenced Cargo targets: ${unusedTargets.join(", ")}`);

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
      `unregistered: ${actualSourceKeys
        .filter((key) => !registeredSourceKeys.includes(key))
        .join(", ")}`,
      `stale: ${registeredSourceKeys
        .filter((key) => !actualSourceKeys.includes(key))
        .join(", ")}`,
    ].join("; "),
  );

  return {
    schema: REGISTRY_SCHEMA,
    registry_count: registry.entries.length,
    source_ignored_count: sourceIgnored.length,
    cargo_target_count: cargoTargets.size,
    platform_counts: Object.fromEntries(
      SUPPORTED_PLATFORMS.map((platform) => [
        platform,
        registry.entries.filter((entry) => entry.platforms.includes(platform)).length,
      ]),
    ),
    packages: sorted(new Set(registry.entries.map((entry) => entry.package))),
    evidence_classes: sorted(new Set(registry.entries.map((entry) => entry.evidence_class))),
  };
}

export function parseCargoTestArtifacts(stdout, { metadata, repoRoot }) {
  const members = new Set(metadata.workspace_members ?? []);
  const packagesById = new Map(
    (metadata.packages ?? [])
      .filter((candidate) => members.has(candidate.id))
      .map((candidate) => [candidate.id, candidate]),
  );
  const artifacts = new Map();

  for (const [index, line] of stdout.split(/\r?\n/).entries()) {
    if (!line.trim()) continue;
    let message;
    try {
      message = JSON.parse(line);
    } catch (error) {
      throw new Error(`Cargo JSON line ${index + 1} is invalid: ${error.message}`);
    }
    if (
      message.reason !== "compiler-artifact" ||
      message.profile?.test !== true ||
      typeof message.executable !== "string"
    ) {
      continue;
    }
    const workspacePackage = packagesById.get(message.package_id);
    if (!workspacePackage) continue;
    const identity = metadataTargetIdentity(
      workspacePackage.name,
      message.target,
      repoRoot,
    );
    const metadataMatch = workspacePackage.targets.some(
      (candidate) =>
        targetIdentityKey(metadataTargetIdentity(workspacePackage.name, candidate, repoRoot)) ===
        targetIdentityKey(identity),
    );
    assert(
      metadataMatch,
      `Cargo test artifact has unknown target identity ${targetIdentityKey(identity)}`,
    );
    artifacts.set(`${targetIdentityKey(identity)}\u0000${message.executable}`, {
      executable: message.executable,
      target: identity,
    });
  }

  return [...artifacts.values()].sort((left, right) =>
    `${targetIdentityKey(left.target)}:${left.executable}`.localeCompare(
      `${targetIdentityKey(right.target)}:${right.executable}`,
    ),
  );
}

export function parseTestHarnessIgnoredList(stdout) {
  const ids = [];
  for (const line of stdout.split(/\r?\n/)) {
    const match = line.match(/^(.+): (test|benchmark)$/);
    if (!match) continue;
    assert(
      match[2] === "test",
      `ignored benchmark is not representable in the test registry: ${match[1]}`,
    );
    ids.push(match[1]);
  }
  return ids;
}

export function expectedCargoInventory(registry, platformName = process.platform) {
  const platform = registryPlatform(platformName);
  const targets = new Map(
    registry.cargo_targets.map((target) => [
      targetReference(target.package, target.selector),
      target,
    ]),
  );
  return registry.entries
    .filter((entry) => entry.platforms.includes(platform))
    .map((entry) => {
      const target = targets.get(targetReference(entry.package, entry.target));
      assert(target, `${entry.test_id}: registry target is missing`);
      return { test_id: entry.test_id, target: targetIdentity(target) };
    })
    .sort((left, right) => describeInventory(left).localeCompare(describeInventory(right)));
}

export function reconcileCargoInventory(
  registry,
  actualInventory,
  platformName = process.platform,
) {
  const platform = registryPlatform(platformName);
  const expectedInventory = expectedCargoInventory(registry, platform);
  const expectedCounts = counts(expectedInventory.map(inventoryKey));
  const actualCounts = counts(actualInventory.map(inventoryKey));
  const expectedByKey = new Map(
    expectedInventory.map((entry) => [inventoryKey(entry), entry]),
  );
  const actualByKey = new Map(
    actualInventory.map((entry) => [inventoryKey(entry), entry]),
  );

  const unknown = [];
  for (const [key, count] of actualCounts) {
    for (let index = expectedCounts.get(key) ?? 0; index < count; index += 1) {
      unknown.push(describeInventory(actualByKey.get(key)));
    }
  }
  const missing = [];
  for (const [key, count] of expectedCounts) {
    for (let index = actualCounts.get(key) ?? 0; index < count; index += 1) {
      missing.push(describeInventory(expectedByKey.get(key)));
    }
  }
  unknown.sort((left, right) => left.localeCompare(right));
  missing.sort((left, right) => left.localeCompare(right));
  assert(
    unknown.length === 0 && missing.length === 0,
    [
      `Cargo ignored inventory mismatch on ${platform}`,
      `unregistered or unavailable: ${unknown.join(", ")}`,
      `missing: ${missing.join(", ")}`,
    ].join("; "),
  );

  return {
    platform,
    cargo_count: actualInventory.length,
    expected_platform_count: expectedInventory.length,
    registry_count: registry.expected_test_count,
    exact_current_platform_match: true,
  };
}

export function collectCargoIgnoredInventory({
  repoRoot,
  metadata,
  cargo = process.env.CARGO ?? "cargo",
}) {
  const build = spawnSync(cargo, CARGO_BUILD_ARGUMENTS, {
    cwd: repoRoot,
    encoding: "utf8",
    maxBuffer: 256 * 1024 * 1024,
  });
  assert(!build.error, `Cargo test-list build failed to start: ${build.error?.message}`);
  assert(build.status === 0, build.stderr || "Cargo test-list build failed");
  const artifacts = parseCargoTestArtifacts(build.stdout, { metadata, repoRoot });
  assert(artifacts.length > 0, "Cargo produced no workspace test executables");

  const inventory = [];
  for (const artifact of artifacts) {
    const listed = spawnSync(artifact.executable, TEST_HARNESS_LIST_ARGUMENTS, {
      cwd: repoRoot,
      encoding: "utf8",
      maxBuffer: 64 * 1024 * 1024,
    });
    assert(
      !listed.error,
      `test harness failed to start: ${artifact.executable}: ${listed.error?.message}`,
    );
    assert(
      listed.status === 0,
      listed.stderr || `test harness list failed: ${artifact.executable}`,
    );
    for (const testId of parseTestHarnessIgnoredList(listed.stdout)) {
      inventory.push({ test_id: testId, target: artifact.target });
    }
  }
  return inventory.sort((left, right) =>
    describeInventory(left).localeCompare(describeInventory(right)),
  );
}

export function readMetadata(repoRoot) {
  const result = spawnSync(
    process.env.CARGO ?? "cargo",
    ["metadata", "--locked", "--no-deps", "--format-version", "1"],
    { cwd: repoRoot, encoding: "utf8", maxBuffer: 64 * 1024 * 1024 },
  );
  assert(!result.error, `cargo metadata failed to start: ${result.error?.message}`);
  assert(result.status === 0, result.stderr || "cargo metadata failed");
  return JSON.parse(result.stdout);
}

function main() {
  const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
  const registry = JSON.parse(
    fs.readFileSync(path.join(repoRoot, "ci/ignored-tests.json"), "utf8"),
  );
  const metadata = readMetadata(repoRoot);
  const report = validateRegistry({ registry, metadata, repoRoot });

  if (process.argv.includes("--cargo-list")) {
    const actualInventory = collectCargoIgnoredInventory({ repoRoot, metadata });
    report.cargo_reconciliation = reconcileCargoInventory(registry, actualInventory);
    report.cargo_reconciliation.no_test_execution = true;
  }

  console.log(JSON.stringify(report, null, 2));
}

if (
  process.argv[1] &&
  path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
  main();
}
