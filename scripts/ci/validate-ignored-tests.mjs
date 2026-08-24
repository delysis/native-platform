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
export const TEST_HARNESS_TIMEOUT_MS = 30_000;

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

export function assertNoCustomHarnessManifest(manifestSource, label = "Cargo.toml") {
  assert(
    !/\\(?:u[0-9A-Fa-f]{4}|U[0-9A-Fa-f]{8})/.test(manifestSource),
    `${label}: TOML Unicode escapes are prohibited by the default-libtest policy`,
  );
  assert(
    !/(^|[^A-Za-z0-9_-])harness(?=$|[^A-Za-z0-9_-])/.test(manifestSource),
    `${label}: explicit Cargo harness configuration is prohibited; standard libtest is required`,
  );
}

function validateWorkspaceLibtestManifests(metadata, repoRoot) {
  const manifests = new Set(
    [...workspacePackages(metadata).values()].map((candidate) => candidate.manifest_path),
  );
  for (const manifest of manifests) {
    const relative = repoRelative(repoRoot, manifest);
    assertNoCustomHarnessManifest(fs.readFileSync(manifest, "utf8"), relative);
  }
  return manifests.size;
}

function validateCargoTargets(registry, metadata, repoRoot) {
  assert(Array.isArray(registry.cargo_targets), "cargo_targets must be an array");
  const packages = workspacePackages(metadata);
  const references = new Map();
  const identities = [];

  for (const target of registry.cargo_targets) {
    for (const field of [
      "package",
      "selector",
      "name",
      "src_path",
      "manifest_path",
      "harness",
    ]) {
      assert(
        typeof target[field] === "string" && target[field].trim(),
        `Cargo target is missing ${field}`,
      );
    }
    assert(
      target.harness === "libtest",
      `${target.package}:${target.selector}: only the standard libtest harness is supported`,
    );
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
    const [metadataTarget] = matches;
    assert(
      metadataTarget.test === true,
      `${reference}: Cargo metadata does not enable this target for standard tests`,
    );
    assert(
      !metadataTarget.kind.includes("custom-build"),
      `${reference}: build-script targets cannot own ignored libtest entries`,
    );
    if (target.selector === "lib") {
      assert(
        metadataTarget.kind.some((kind) =>
          ["lib", "rlib", "dylib", "cdylib", "staticlib", "proc-macro"].includes(kind),
        ),
        `${reference}: lib selector does not resolve to a library target`,
      );
    } else {
      assert(
        target.selector === `test:${target.name}` &&
          JSON.stringify(metadataTarget.kind) === JSON.stringify(["test"]),
        `${reference}: only exact test:<name> integration selectors are supported`,
      );
    }

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

function isRustIdentifierStart(character) {
  return character !== undefined && /[A-Za-z_]/.test(character);
}

function isRustIdentifierContinue(character) {
  return character !== undefined && /[A-Za-z0-9_]/.test(character);
}

function rawStringPrefixLength(source, offset) {
  if (source[offset] === "r") return 1;
  if (["b", "c"].includes(source[offset]) && source[offset + 1] === "r") return 2;
  return 0;
}

function tokenizeRust(source, label) {
  const tokens = [];
  let offset = 0;
  while (offset < source.length) {
    const character = source[offset];
    if (/\s/.test(character)) {
      offset += 1;
      continue;
    }
    if (source.startsWith("//", offset)) {
      offset = source.indexOf("\n", offset + 2);
      if (offset === -1) break;
      continue;
    }
    if (source.startsWith("/*", offset)) {
      let depth = 1;
      offset += 2;
      while (offset < source.length && depth > 0) {
        if (source.startsWith("/*", offset)) {
          depth += 1;
          offset += 2;
        } else if (source.startsWith("*/", offset)) {
          depth -= 1;
          offset += 2;
        } else {
          offset += 1;
        }
      }
      assert(depth === 0, `${label}: unterminated Rust block comment`);
      continue;
    }

    const rawPrefix = rawStringPrefixLength(source, offset);
    if (rawPrefix > 0) {
      let cursor = offset + rawPrefix;
      let hashes = 0;
      while (source[cursor] === "#") {
        hashes += 1;
        cursor += 1;
      }
      if (source[cursor] === '"') {
        const terminator = `"${"#".repeat(hashes)}`;
        const end = source.indexOf(terminator, cursor + 1);
        assert(end !== -1, `${label}: unterminated Rust raw string`);
        tokens.push({ kind: "string", value: "string" });
        offset = end + terminator.length;
        continue;
      }
    }

    const stringPrefix = ["b", "c"].includes(character) && source[offset + 1] === '"' ? 1 : 0;
    if (character === '"' || stringPrefix > 0) {
      let cursor = offset + stringPrefix + 1;
      let closed = false;
      while (cursor < source.length) {
        if (source[cursor] === "\\") {
          cursor += 2;
        } else if (source[cursor] === '"') {
          cursor += 1;
          closed = true;
          break;
        } else {
          cursor += 1;
        }
      }
      assert(closed, `${label}: unterminated Rust string`);
      tokens.push({ kind: "string", value: "string" });
      offset = cursor;
      continue;
    }

    const charPrefix = character === "b" && source[offset + 1] === "'" ? 1 : 0;
    if (character === "'" || charPrefix > 0) {
      const start = offset + charPrefix;
      let cursor = start + 1;
      if (source[cursor] === "\\") cursor += 2;
      else cursor += 1;
      if (source[cursor] === "'") {
        tokens.push({ kind: "char", value: "char" });
        offset = cursor + 1;
        continue;
      }
    }

    if (
      character === "r" &&
      source[offset + 1] === "#" &&
      isRustIdentifierStart(source[offset + 2])
    ) {
      let cursor = offset + 3;
      while (isRustIdentifierContinue(source[cursor])) cursor += 1;
      tokens.push({
        kind: "identifier",
        value: source.slice(offset + 2, cursor),
        raw: true,
      });
      offset = cursor;
      continue;
    }
    if (isRustIdentifierStart(character)) {
      let cursor = offset + 1;
      while (isRustIdentifierContinue(source[cursor])) cursor += 1;
      tokens.push({
        kind: "identifier",
        value: source.slice(offset, cursor),
        raw: false,
      });
      offset = cursor;
      continue;
    }

    tokens.push({ kind: "punctuation", value: character });
    offset += 1;
  }
  return tokens;
}

function rustAttributes(tokens, label) {
  const attributes = [];
  for (let index = 0; index < tokens.length; index += 1) {
    if (tokens[index].value !== "#") continue;
    let cursor = index + 1;
    const inner = tokens[cursor]?.value === "!";
    if (inner) cursor += 1;
    if (tokens[cursor]?.value !== "[") continue;

    let depth = 1;
    const contents = [];
    cursor += 1;
    for (; cursor < tokens.length && depth > 0; cursor += 1) {
      if (tokens[cursor].value === "[") depth += 1;
      if (tokens[cursor].value === "]") depth -= 1;
      if (depth > 0) contents.push(tokens[cursor]);
    }
    assert(depth === 0, `${label}: unterminated Rust attribute`);

    const identifiers = contents.filter((token) => token.kind === "identifier");
    const head = identifiers[0];
    const pathIdentifiers = [];
    for (const token of contents) {
      if (["(", "="].includes(token.value)) break;
      if (token.kind === "identifier") pathIdentifiers.push(token);
    }
    const canonicalIgnore =
      !inner &&
      contents.length === 3 &&
      head?.value === "ignore" &&
      head.raw === false &&
      contents[1].value === "=" &&
      contents[2].kind === "string";
    attributes.push({
      start: index,
      end: cursor - 1,
      inner,
      canonical_ignore: canonicalIgnore,
      contains_ignore: identifiers.some((token) => token.value === "ignore"),
      test_marker:
        (JSON.stringify(pathIdentifiers.map((token) => token.value)) ===
          JSON.stringify(["test"]) ||
          JSON.stringify(pathIdentifiers.map((token) => token.value)) ===
            JSON.stringify(["tokio", "test"])) &&
        pathIdentifiers.every((token) => token.raw === false),
    });
    index = cursor - 1;
  }
  return attributes;
}

function macroTokenRanges(tokens, label) {
  const closing = { "(": ")", "[": "]", "{": "}" };
  const ranges = [];
  for (let index = 0; index < tokens.length; index += 1) {
    if (tokens[index].value !== "!") continue;
    let open = index + 1;
    if (
      tokens[index - 1]?.value === "macro_rules" &&
      tokens[open]?.kind === "identifier"
    ) {
      open += 1;
    }
    const expectedClose = closing[tokens[open]?.value];
    if (!expectedClose) continue;

    const stack = [expectedClose];
    let cursor = open + 1;
    for (; cursor < tokens.length && stack.length > 0; cursor += 1) {
      const nextClose = closing[tokens[cursor].value];
      if (nextClose) stack.push(nextClose);
      else if (tokens[cursor].value === stack.at(-1)) stack.pop();
    }
    assert(stack.length === 0, `${label}: unterminated Rust macro token tree`);
    ranges.push({ start: open, end: cursor - 1 });
  }
  return ranges;
}

export function discoverCanonicalIgnoredTests(source, label = "Rust source") {
  const tokens = tokenizeRust(source, label);
  const attributes = rustAttributes(tokens, label);
  const macroRanges = macroTokenRanges(tokens, label);
  const groups = [];
  for (const attribute of attributes) {
    const current = groups.at(-1);
    if (current && current.at(-1).end + 1 === attribute.start) current.push(attribute);
    else groups.push([attribute]);
  }

  const tests = [];
  for (const group of groups) {
    if (!group.some((attribute) => attribute.contains_ignore)) continue;
    assert(
      !macroRanges.some(
        (range) => group[0].start > range.start && group[0].start < range.end,
      ),
      `${label}: macro-generated ignored tests are prohibited`,
    );
    for (const attribute of group) {
      assert(
        !attribute.contains_ignore || attribute.canonical_ignore,
        `${label}: noncanonical ignore syntax is prohibited`,
      );
    }
    const ignored = group.filter((attribute) => attribute.canonical_ignore);
    assert(ignored.length === 1, `${label}: exactly one canonical ignore attribute is required`);
    assert(
      group.some((attribute) => attribute.test_marker),
      `${label}: canonical ignore must accompany an explicit test attribute`,
    );

    let cursor = group.at(-1).end + 1;
    assert(tokens[cursor]?.value !== "pub", `${label}: ignored tests must be private functions`);
    if (tokens[cursor]?.value === "async") cursor += 1;
    assert(
      tokens[cursor]?.value === "fn" &&
        tokens[cursor + 1]?.kind === "identifier" &&
        tokens[cursor + 1]?.raw === false,
      `${label}: canonical ignore must annotate a private fn or async fn`,
    );
    tests.push(tokens[cursor + 1].value);
  }
  return tests;
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
  for (const source of rustFiles) {
    const sourceText = fs.readFileSync(source, "utf8");
    const relative = repoRelative(repoRoot, source);
    for (const functionName of discoverCanonicalIgnoredTests(sourceText, relative)) {
      results.push({
        source: relative,
        function_name: functionName,
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
  const libtestManifestCount = validateWorkspaceLibtestManifests(metadata, repoRoot);
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
    assert(
      discoverCanonicalIgnoredTests(sourceText, entry.source).includes(functionName),
      `${entry.test_id}: source does not contain the canonical ignored function`,
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
    default_libtest_manifest_count: libtestManifestCount,
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

export function selectStandardLibtestArtifacts(artifacts, { metadata, repoRoot }) {
  const safeTargetIdentities = new Set();
  for (const workspacePackage of workspacePackages(metadata).values()) {
    for (const target of workspacePackage.targets) {
      if (target.test !== true) continue;
      if (target.kind.includes("custom-build")) continue;
      safeTargetIdentities.add(
        targetIdentityKey(
          metadataTargetIdentity(workspacePackage.name, target, repoRoot),
        ),
      );
    }
  }

  const selected = artifacts.filter((artifact) =>
    safeTargetIdentities.has(targetIdentityKey(artifact.target)),
  );
  const duplicateTargets = duplicates(
    selected.map((artifact) => targetIdentityKey(artifact.target)),
  );
  assert(
    duplicateTargets.length === 0,
    `Cargo produced multiple executables for a standard libtest target: ${duplicateTargets.join(", ")}`,
  );
  return selected;
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
  registry,
  cargo = process.env.CARGO ?? "cargo",
}) {
  validateWorkspaceLibtestManifests(metadata, repoRoot);
  validateCargoTargets(registry, metadata, repoRoot);
  const build = spawnSync(cargo, CARGO_BUILD_ARGUMENTS, {
    cwd: repoRoot,
    encoding: "utf8",
    maxBuffer: 256 * 1024 * 1024,
  });
  assert(!build.error, `Cargo test-list build failed to start: ${build.error?.message}`);
  assert(build.status === 0, build.stderr || "Cargo test-list build failed");
  const artifacts = parseCargoTestArtifacts(build.stdout, { metadata, repoRoot });
  assert(artifacts.length > 0, "Cargo produced no workspace test executables");
  const standardLibtestArtifacts = selectStandardLibtestArtifacts(artifacts, {
    metadata,
    repoRoot,
  });
  assert(
    standardLibtestArtifacts.length > 0,
    "Cargo produced no standard libtest executables",
  );

  const inventory = [];
  for (const artifact of standardLibtestArtifacts) {
    const listed = spawnSync(artifact.executable, TEST_HARNESS_LIST_ARGUMENTS, {
      cwd: repoRoot,
      encoding: "utf8",
      maxBuffer: 64 * 1024 * 1024,
      timeout: TEST_HARNESS_TIMEOUT_MS,
    });
    assert(
      listed.error?.code !== "ETIMEDOUT",
      `standard libtest harness list timed out after ${TEST_HARNESS_TIMEOUT_MS}ms: ${artifact.executable}`,
    );
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
  inventory.sort((left, right) =>
    describeInventory(left).localeCompare(describeInventory(right)),
  );
  return {
    inventory,
    cargo_test_artifact_count: artifacts.length,
    listed_standard_libtest_harness_count: standardLibtestArtifacts.length,
  };
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
    const collection = collectCargoIgnoredInventory({ repoRoot, metadata, registry });
    report.cargo_reconciliation = reconcileCargoInventory(
      registry,
      collection.inventory,
    );
    report.cargo_reconciliation.cargo_test_artifact_count =
      collection.cargo_test_artifact_count;
    report.cargo_reconciliation.listed_standard_libtest_harness_count =
      collection.listed_standard_libtest_harness_count;
    report.cargo_reconciliation.harness_timeout_ms = TEST_HARNESS_TIMEOUT_MS;
    report.cargo_reconciliation.harness_list_only = true;
    report.cargo_reconciliation.no_test_body_execution = true;
  }

  console.log(JSON.stringify(report, null, 2));
}

if (
  process.argv[1] &&
  path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
  main();
}
