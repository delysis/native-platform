#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
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

const CUSTOM_TEST_FRAMEWORK_ATTRIBUTES = new Set([
  "crate_type",
  "custom_test_frameworks",
  "no_main",
  "reexport_test_harness_main",
  "test_runner",
]);
const FORBIDDEN_HARNESS_ENVIRONMENT = new Set([
  "CARGO",
  "CARGO_ENCODED_RUSTDOCFLAGS",
  "CARGO_ENCODED_RUSTFLAGS",
  "CARGO_TARGET_DIR",
  "DYLD_FRAMEWORK_PATH",
  "DYLD_INSERT_LIBRARIES",
  "DYLD_LIBRARY_PATH",
  "LD_LIBRARY_PATH",
  "LD_PRELOAD",
  "RUSTC",
  "RUSTC_BOOTSTRAP",
  "RUSTC_WORKSPACE_WRAPPER",
  "RUSTC_WRAPPER",
  "RUSTDOC",
  "RUSTDOCFLAGS",
  "RUSTFLAGS",
]);
const FORBIDDEN_CARGO_CONFIG_WORDS = [
  "RUSTC",
  "RUSTC_BOOTSTRAP",
  "RUSTDOC",
  "RUSTDOCFLAGS",
  "RUSTFLAGS",
  "linker",
  "paths",
  "replace-with",
  "runner",
  "rustc",
  "rustc-workspace-wrapper",
  "rustc-wrapper",
  "rustdoc",
  "rustdocflags",
  "rustflags",
  "source",
  "target",
  "target-dir",
];
const STANDARD_LIBTEST_GUARD = Symbol("standard-libtest-guard");

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

function sha256(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
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

export function assertSafeHarnessEnvironment(environment = process.env) {
  const forbidden = [];
  for (const [name, value] of Object.entries(environment)) {
    if (value === undefined || value === "") continue;
    const canonical = name.toUpperCase();
    if (
      FORBIDDEN_HARNESS_ENVIRONMENT.has(canonical) ||
      canonical.startsWith("DYLD_") ||
      /^CARGO_BUILD_(?:RUSTC(?:_WORKSPACE_WRAPPER|_WRAPPER)?|RUSTDOC|RUSTDOCFLAGS|RUSTFLAGS)$/.test(
        canonical,
      ) ||
      canonical === "CARGO_BUILD_TARGET" ||
      /^CARGO_TARGET_[A-Z0-9_]+_(?:LINKER|RUNNER|RUSTDOCFLAGS|RUSTFLAGS)$/.test(
        canonical,
      )
    ) {
      forbidden.push(name);
    }
  }
  assert(
    forbidden.length === 0,
    `standard-libtest guard rejects compiler, runner, loader, bootstrap, target, and flag environment overrides: ${sorted(forbidden).join(", ")}`,
  );
}

export function assertSafeCargoConfig(configSource, label = ".cargo/config.toml") {
  assert(
    !/\\(?:u[0-9A-Fa-f]{4}|U[0-9A-Fa-f]{8})/.test(configSource),
    `${label}: TOML Unicode escapes are prohibited by the standard-libtest policy`,
  );
  for (const word of FORBIDDEN_CARGO_CONFIG_WORDS) {
    const escaped = word.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    assert(
      !new RegExp(`(^|[^A-Za-z0-9_-])${escaped}(?=$|[^A-Za-z0-9_-])`).test(
        configSource,
      ),
      `${label}: compiler, runner, bootstrap, rustflags, and rustdocflags configuration is prohibited (${word})`,
    );
  }
}

function cargoConfigurationCandidates(repoRoot, environment) {
  const candidates = new Set();
  let directory = path.resolve(repoRoot);
  while (true) {
    candidates.add(path.join(directory, ".cargo", "config"));
    candidates.add(path.join(directory, ".cargo", "config.toml"));
    const parent = path.dirname(directory);
    if (parent === directory) break;
    directory = parent;
  }

  const configuredCargoHome = environment.CARGO_HOME;
  const home = environment.HOME ?? environment.USERPROFILE ?? os.homedir();
  const cargoHome = configuredCargoHome
    ? path.resolve(repoRoot, configuredCargoHome)
    : path.join(home, ".cargo");
  candidates.add(path.join(cargoHome, "config"));
  candidates.add(path.join(cargoHome, "config.toml"));
  return sorted(candidates);
}

function validateCargoConfiguration(repoRoot, environment) {
  const configs = cargoConfigurationCandidates(repoRoot, environment).filter((candidate) =>
    fs.existsSync(candidate),
  );
  for (const config of configs) {
    assertSafeCargoConfig(fs.readFileSync(config, "utf8"), config);
  }
  return configs;
}

export function parsePinnedRustToolchain(source, label = "rust-toolchain.toml") {
  assert(
    !/\\(?:u[0-9A-Fa-f]{4}|U[0-9A-Fa-f]{8})/.test(source),
    `${label}: TOML Unicode escapes are prohibited in the pinned toolchain`,
  );
  const channels = [...source.matchAll(/^\s*channel\s*=\s*"([^"]+)"\s*$/gm)].map(
    (match) => match[1],
  );
  assert(channels.length === 1, `${label}: exactly one toolchain channel is required`);
  assert(
    /^\d+\.\d+\.\d+$/.test(channels[0]),
    `${label}: toolchain channel must be an exact stable Rust version`,
  );
  return channels[0];
}

function pinnedRustToolchain(repoRoot) {
  const toolchainPath = path.join(repoRoot, "rust-toolchain.toml");
  assert(fs.existsSync(toolchainPath), "rust-toolchain.toml is required");
  return parsePinnedRustToolchain(fs.readFileSync(toolchainPath, "utf8"));
}

function runPinnedRustTool(tool, arguments_, { repoRoot, environment }) {
  const channel = pinnedRustToolchain(repoRoot);
  return spawnSync("rustup", ["run", channel, tool, ...arguments_], {
    cwd: repoRoot,
    encoding: "utf8",
    env: environment,
    maxBuffer: 256 * 1024 * 1024,
  });
}

function pinnedToolPath(tool, channel, { repoRoot, environment }) {
  const result = spawnSync(
    "rustup",
    ["which", "--toolchain", channel, tool],
    {
      cwd: repoRoot,
      encoding: "utf8",
      env: environment,
      maxBuffer: 1024 * 1024,
    },
  );
  assert(!result.error, `rustup could not resolve pinned ${tool}: ${result.error?.message}`);
  assert(result.status === 0, result.stderr || `rustup could not resolve pinned ${tool}`);
  const resolved = result.stdout.trim();
  assert(path.isAbsolute(resolved), `rustup returned a non-absolute ${tool} path`);
  const real = fs.realpathSync(resolved);
  assert(fs.statSync(real).isFile(), `pinned ${tool} is not a regular file: ${real}`);
  return real;
}

function pinnedToolVersion(tool, channel, { repoRoot, environment }) {
  const result = runPinnedRustTool(tool, ["--version", "--verbose"], {
    repoRoot,
    environment,
  });
  assert(!result.error, `pinned ${tool} identity failed to start: ${result.error?.message}`);
  assert(result.status === 0, result.stderr || `pinned ${tool} identity failed`);
  const firstLine = result.stdout.split(/\r?\n/, 1)[0];
  assert(
    firstLine.startsWith(`${tool} ${channel} `) || firstLine === `${tool} ${channel}`,
    `pinned ${tool} reports ${firstLine}, expected ${channel}`,
  );
  return firstLine;
}

export function readPinnedToolIdentity({
  repoRoot,
  environment = process.env,
}) {
  assertSafeHarnessEnvironment(environment);
  validateCargoConfiguration(repoRoot, environment);
  const channel = pinnedRustToolchain(repoRoot);
  return {
    channel,
    cargo_path: pinnedToolPath("cargo", channel, { repoRoot, environment }),
    cargo_version: pinnedToolVersion("cargo", channel, {
      repoRoot,
      environment,
    }),
    rustc_path: pinnedToolPath("rustc", channel, { repoRoot, environment }),
    rustc_version: pinnedToolVersion("rustc", channel, {
      repoRoot,
      environment,
    }),
  };
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

function workspaceCustomBuildTargets(metadata, repoRoot) {
  const targets = [];
  for (const workspacePackage of workspacePackages(metadata).values()) {
    for (const target of workspacePackage.targets) {
      if (!target.kind.includes("custom-build")) continue;
      targets.push({
        package: workspacePackage.name,
        src_path: repoRelative(repoRoot, target.src_path),
      });
    }
  }
  return targets.sort((left, right) =>
    `${left.package}:${left.src_path}`.localeCompare(`${right.package}:${right.src_path}`),
  );
}

function assertReviewedBuildScriptSource(source, label) {
  for (const directive of [
    "rustc-cfg",
    "rustc-flags",
    "rustc-link-arg-tests",
  ]) {
    assert(
      !source.includes(directive),
      `${label}: reviewed build scripts may not emit ${directive}`,
    );
  }
}

function validateReviewedBuildScripts(registry, metadata, repoRoot) {
  assert(
    Array.isArray(registry.reviewed_build_scripts),
    "reviewed_build_scripts must be an array",
  );
  const reviewed = [];
  for (const entry of registry.reviewed_build_scripts) {
    for (const field of ["package", "src_path", "sha256"]) {
      assert(
        typeof entry[field] === "string" && entry[field].trim(),
        `reviewed build script is missing ${field}`,
      );
    }
    assert(
      /^[0-9a-f]{64}$/.test(entry.sha256),
      `${entry.package}:${entry.src_path}: invalid reviewed build-script SHA-256`,
    );
    const sourcePath = path.resolve(repoRoot, entry.src_path);
    assert(
      repoRelative(repoRoot, sourcePath) === entry.src_path && fs.existsSync(sourcePath),
      `${entry.package}:${entry.src_path}: reviewed build script is missing`,
    );
    const source = fs.readFileSync(sourcePath);
    assertReviewedBuildScriptSource(source.toString("utf8"), entry.src_path);
    const actual = sha256(source);
    assert(
      actual === entry.sha256,
      `${entry.package}:${entry.src_path}: build-script digest ${actual} != reviewed ${entry.sha256}`,
    );
    reviewed.push({ package: entry.package, src_path: entry.src_path });
  }

  const actual = workspaceCustomBuildTargets(metadata, repoRoot);
  const canonicalReviewed = reviewed.sort((left, right) =>
    `${left.package}:${left.src_path}`.localeCompare(`${right.package}:${right.src_path}`),
  );
  assert(
    JSON.stringify(canonicalReviewed) === JSON.stringify(actual),
    [
      "workspace build scripts differ from the reviewed standard-libtest set",
      `unreviewed: ${actual
        .filter(
          (candidate) =>
            !canonicalReviewed.some(
              (entry) =>
                entry.package === candidate.package && entry.src_path === candidate.src_path,
            ),
        )
        .map((entry) => `${entry.package}:${entry.src_path}`)
        .join(", ")}`,
      `stale: ${canonicalReviewed
        .filter(
          (entry) =>
            !actual.some(
              (candidate) =>
                candidate.package === entry.package && candidate.src_path === entry.src_path,
            ),
        )
        .map((entry) => `${entry.package}:${entry.src_path}`)
        .join(", ")}`,
    ].join("; "),
  );
  return actual.length;
}

function validateNoWorkspaceProcMacros(metadata) {
  const procMacros = [];
  for (const workspacePackage of workspacePackages(metadata).values()) {
    for (const target of workspacePackage.targets) {
      if (target.kind.includes("proc-macro")) {
        procMacros.push(`${workspacePackage.name}:${target.name}`);
      }
    }
  }
  assert(
    procMacros.length === 0,
    `workspace procedural macros are outside the standard-libtest guard boundary: ${procMacros.join(", ")}`,
  );
  return procMacros.length;
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
          ["lib", "rlib", "dylib", "cdylib", "staticlib"].includes(kind),
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
      identifiers,
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

export function assertStandardLibtestRustSource(source, label = "Rust source") {
  const tokens = tokenizeRust(source, label);
  const attributes = rustAttributes(tokens, label);
  const macroRanges = macroTokenRanges(tokens, label);

  for (const attribute of attributes) {
    const forbidden = attribute.identifiers
      .map((token) => token.value)
      .filter((identifier) => CUSTOM_TEST_FRAMEWORK_ATTRIBUTES.has(identifier));
    assert(
      forbidden.length === 0,
      `${label}: custom test-framework crate attributes are prohibited (${sorted(new Set(forbidden)).join(", ")})`,
    );
    assert(
      !(
        attribute.inner &&
        macroRanges.some(
          (range) => attribute.start > range.start && attribute.start < range.end,
        )
      ),
      `${label}: macro-generated crate attributes are outside the standard-libtest guard boundary`,
    );
  }

  for (let index = 0; index < tokens.length - 1; index += 1) {
    assert(
      !(
        tokens[index].kind === "identifier" &&
        tokens[index].value === "include" &&
        tokens[index + 1].value === "!"
      ),
      `${label}: include! can inject generated Rust outside the standard-libtest source guard`,
    );
  }
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

function workspaceRustFiles(packageRoots) {
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
  return sorted(rustFiles);
}

function ignoredTestsInSource(repoRoot, packageRoots) {
  const rustFiles = workspaceRustFiles(packageRoots);

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

function standardLibtestSnapshotPaths(repoRoot, metadata, rustFiles, cargoConfigs) {
  const files = new Set(rustFiles.map((candidate) => path.resolve(candidate)));
  for (const workspacePackage of workspacePackages(metadata).values()) {
    files.add(path.resolve(workspacePackage.manifest_path));
  }
  for (const relative of [
    "Cargo.lock",
    "Cargo.toml",
    "ci/ignored-tests.json",
    "rust-toolchain",
    "rust-toolchain.toml",
  ]) {
    const candidate = path.resolve(repoRoot, relative);
    if (fs.existsSync(candidate)) files.add(candidate);
  }
  for (const config of cargoConfigs) files.add(path.resolve(config));
  return sorted(files);
}

function standardLibtestSnapshot(paths) {
  return paths.map((candidate) => [candidate, sha256(fs.readFileSync(candidate))]);
}

export function createStandardLibtestGuard({
  registry,
  metadata,
  repoRoot,
  environment = process.env,
}) {
  assertSafeHarnessEnvironment(environment);
  const cargoConfigs = validateCargoConfiguration(repoRoot, environment);
  const manifestCount = validateWorkspaceLibtestManifests(metadata, repoRoot);
  const reviewedBuildScriptCount = validateReviewedBuildScripts(
    registry,
    metadata,
    repoRoot,
  );
  const workspaceProcMacroCount = validateNoWorkspaceProcMacros(metadata);
  const rustFiles = workspaceRustFiles(workspacePackageRoots(metadata));
  for (const source of rustFiles) {
    assertStandardLibtestRustSource(
      fs.readFileSync(source, "utf8"),
      repoRelative(repoRoot, source),
    );
  }
  const snapshotPaths = standardLibtestSnapshotPaths(
    repoRoot,
    metadata,
    rustFiles,
    cargoConfigs,
  );
  return Object.freeze({
    [STANDARD_LIBTEST_GUARD]: true,
    repo_root: path.resolve(repoRoot),
    cargo_config_count: cargoConfigs.length,
    default_libtest_manifest_count: manifestCount,
    reviewed_build_script_count: reviewedBuildScriptCount,
    standard_libtest_source_count: rustFiles.length,
    workspace_proc_macro_count: workspaceProcMacroCount,
    snapshot: standardLibtestSnapshot(snapshotPaths),
    validated_after_cargo_build: false,
  });
}

export function validateStandardLibtestGuardAfterBuild({
  guard,
  registry,
  metadata,
  repoRoot,
  environment,
}) {
  assert(
    guard?.[STANDARD_LIBTEST_GUARD] === true && guard.repo_root === path.resolve(repoRoot),
    "standard-libtest guard is missing or belongs to a different workspace",
  );
  const current = createStandardLibtestGuard({
    registry,
    metadata,
    repoRoot,
    environment,
  });
  assert(
    JSON.stringify(current.snapshot) === JSON.stringify(guard.snapshot),
    "standard-libtest source, manifest, lock, toolchain, registry, or Cargo configuration changed during the Cargo build",
  );
  return Object.freeze({
    ...current,
    [STANDARD_LIBTEST_GUARD]: true,
    validated_after_cargo_build: true,
  });
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

export function validateRegistry({
  registry,
  metadata,
  repoRoot,
  environment = process.env,
}) {
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
  const standardLibtestGuard = createStandardLibtestGuard({
    registry,
    metadata,
    repoRoot,
    environment,
  });
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
    cargo_config_count: standardLibtestGuard.cargo_config_count,
    default_libtest_manifest_count:
      standardLibtestGuard.default_libtest_manifest_count,
    reviewed_build_script_count:
      standardLibtestGuard.reviewed_build_script_count,
    standard_libtest_source_count:
      standardLibtestGuard.standard_libtest_source_count,
    workspace_proc_macro_count:
      standardLibtestGuard.workspace_proc_macro_count,
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
      typeof message.fresh !== "boolean" ||
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
      fresh: message.fresh,
      target: identity,
    });
  }

  return [...artifacts.values()].sort((left, right) =>
    `${targetIdentityKey(left.target)}:${left.executable}`.localeCompare(
      `${targetIdentityKey(right.target)}:${right.executable}`,
    ),
  );
}

export function assertSuccessfulCargoBuildFinished(stdout) {
  const finished = [];
  for (const [index, line] of stdout.split(/\r?\n/).entries()) {
    if (!line.trim()) continue;
    let message;
    try {
      message = JSON.parse(line);
    } catch (error) {
      throw new Error(`Cargo JSON line ${index + 1} is invalid: ${error.message}`);
    }
    if (message.reason === "build-finished") finished.push(message.success);
  }
  assert(
    JSON.stringify(finished) === JSON.stringify([true]),
    `Cargo JSON must contain exactly one successful build-finished message; found ${JSON.stringify(finished)}`,
  );
}

export function validateArtifactExecutable(executable, targetDirectory) {
  assert(path.isAbsolute(executable), `Cargo artifact path is not absolute: ${executable}`);
  const direct = fs.lstatSync(executable);
  assert(!direct.isSymbolicLink(), `Cargo artifact must not be a symbolic link: ${executable}`);
  assert(direct.isFile(), `Cargo artifact is not a regular file: ${executable}`);
  const targetRoot = fs.realpathSync(targetDirectory);
  const real = fs.realpathSync(executable);
  const relative = path.relative(targetRoot, real);
  assert(
    relative !== "" && !relative.startsWith(`..${path.sep}`) && !path.isAbsolute(relative),
    `Cargo artifact is outside the metadata target directory: ${executable}`,
  );
  return {
    path: real,
    sha256: sha256(fs.readFileSync(real)),
  };
}

export function selectGuardedTestProfileArtifacts(
  artifacts,
  { metadata, repoRoot, guard },
) {
  assert(
    guard?.[STANDARD_LIBTEST_GUARD] === true &&
      guard.repo_root === path.resolve(repoRoot) &&
      guard.validated_after_cargo_build === true,
    "artifact execution requires an unchanged post-build standard-libtest guard",
  );
  const guardedTargetIdentities = new Set();
  for (const workspacePackage of workspacePackages(metadata).values()) {
    for (const target of workspacePackage.targets) {
      if (target.test !== true) continue;
      if (target.kind.includes("custom-build")) continue;
      if (target.kind.includes("proc-macro")) continue;
      guardedTargetIdentities.add(
        targetIdentityKey(
          metadataTargetIdentity(workspacePackage.name, target, repoRoot),
        ),
      );
    }
  }

  const selected = artifacts.filter((artifact) =>
    guardedTargetIdentities.has(targetIdentityKey(artifact.target)),
  );
  const duplicateTargets = duplicates(
    selected.map((artifact) => targetIdentityKey(artifact.target)),
  );
  assert(
    duplicateTargets.length === 0,
    `Cargo produced multiple executables for a guarded test-profile target: ${duplicateTargets.join(", ")}`,
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
  environment = process.env,
}) {
  const prebuildGuard = createStandardLibtestGuard({
    registry,
    metadata,
    repoRoot,
    environment,
  });
  validateCargoTargets(registry, metadata, repoRoot);
  const tool_identity = readPinnedToolIdentity({ repoRoot, environment });
  const build = runPinnedRustTool("cargo", CARGO_BUILD_ARGUMENTS, {
    repoRoot,
    environment,
  });
  assert(!build.error, `Cargo test-list build failed to start: ${build.error?.message}`);
  assert(build.status === 0, build.stderr || "Cargo test-list build failed");
  assertSuccessfulCargoBuildFinished(build.stdout);
  const artifacts = parseCargoTestArtifacts(build.stdout, { metadata, repoRoot });
  assert(artifacts.length > 0, "Cargo produced no workspace test executables");
  const postbuildGuard = validateStandardLibtestGuardAfterBuild({
    guard: prebuildGuard,
    registry,
    metadata,
    repoRoot,
    environment,
  });
  const guardedArtifacts = selectGuardedTestProfileArtifacts(artifacts, {
    metadata,
    repoRoot,
    guard: postbuildGuard,
  });
  assert(
    guardedArtifacts.length > 0,
    "Cargo produced no guarded test-profile executables",
  );

  const inventory = [];
  const listingCwd = fs.mkdtempSync(path.join(os.tmpdir(), "ignored-list-cwd-"));
  try {
    for (const artifact of guardedArtifacts) {
      const before = validateArtifactExecutable(
        artifact.executable,
        metadata.target_directory,
      );
      const listed = spawnSync(before.path, TEST_HARNESS_LIST_ARGUMENTS, {
        cwd: listingCwd,
        encoding: "utf8",
        env: environment,
        killSignal: "SIGKILL",
        maxBuffer: 64 * 1024 * 1024,
        timeout: TEST_HARNESS_TIMEOUT_MS,
        windowsHide: true,
      });
      assert(
        listed.error?.code !== "ETIMEDOUT",
        `guarded test-profile artifact list timed out after ${TEST_HARNESS_TIMEOUT_MS}ms: ${before.path}`,
      );
      assert(
        !listed.error,
        `guarded test-profile artifact failed to start: ${before.path}: ${listed.error?.message}`,
      );
      assert(
        listed.status === 0,
        listed.stderr || `guarded test-profile artifact list failed: ${before.path}`,
      );
      const after = validateArtifactExecutable(before.path, metadata.target_directory);
      assert(
        after.sha256 === before.sha256,
        `guarded test-profile artifact changed while it was listed: ${before.path}`,
      );
      for (const testId of parseTestHarnessIgnoredList(listed.stdout)) {
        inventory.push({ test_id: testId, target: artifact.target });
      }
    }
  } finally {
    fs.rmSync(listingCwd, { force: true, recursive: true });
  }
  inventory.sort((left, right) =>
    describeInventory(left).localeCompare(describeInventory(right)),
  );
  return {
    inventory,
    cargo_test_artifact_count: artifacts.length,
    guarded_test_profile_artifact_count: guardedArtifacts.length,
    fresh_guarded_artifact_count: guardedArtifacts.filter((artifact) => artifact.fresh).length,
    rebuilt_guarded_artifact_count: guardedArtifacts.filter((artifact) => !artifact.fresh).length,
    standard_libtest_guard_after_build:
      postbuildGuard.validated_after_cargo_build,
    tool_identity,
  };
}

export function readMetadata(repoRoot, environment = process.env) {
  assertSafeHarnessEnvironment(environment);
  validateCargoConfiguration(repoRoot, environment);
  const result = runPinnedRustTool(
    "cargo",
    ["metadata", "--locked", "--no-deps", "--format-version", "1"],
    { repoRoot, environment },
  );
  assert(!result.error, `cargo metadata failed to start: ${result.error?.message}`);
  assert(result.status === 0, result.stderr || "cargo metadata failed");
  return JSON.parse(result.stdout);
}

function main() {
  const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
  assertSafeHarnessEnvironment(process.env);
  validateCargoConfiguration(repoRoot, process.env);
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
    report.cargo_reconciliation.guarded_test_profile_artifact_count =
      collection.guarded_test_profile_artifact_count;
    report.cargo_reconciliation.fresh_guarded_artifact_count =
      collection.fresh_guarded_artifact_count;
    report.cargo_reconciliation.rebuilt_guarded_artifact_count =
      collection.rebuilt_guarded_artifact_count;
    report.cargo_reconciliation.standard_libtest_guard_after_build =
      collection.standard_libtest_guard_after_build;
    report.cargo_reconciliation.tool_identity = collection.tool_identity;
    report.cargo_reconciliation.harness_timeout_ms = TEST_HARNESS_TIMEOUT_MS;
    report.cargo_reconciliation.cargo_rustc_test_mode_requested = true;
    report.cargo_reconciliation.guarded_list_execution = true;
    report.cargo_reconciliation.guarded_harness_arguments =
      TEST_HARNESS_LIST_ARGUMENTS;
  }

  console.log(JSON.stringify(report, null, 2));
}

if (
  process.argv[1] &&
  path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
  main();
}
