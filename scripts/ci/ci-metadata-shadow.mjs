import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";

export const SELECTION_SCHEMA = "native-platform.ci-reverse-closure.v2";
export const SHADOW_SCHEMA = "native-platform.ci-legacy-equivalence.v2";
const KNOWN_EFFECTS = new Set([
  "dependency_graph",
  "force_full",
  "frontend_fte",
  "frontend_loom",
  "frontend_mom",
  "ignored_tests",
  "import",
  "platform_macos",
  "release",
  "root",
]);

function asRepoPath(value) {
  return value.split(path.sep).join("/").replace(/^\.\//, "");
}

function under(candidate, prefix) {
  return candidate === prefix || candidate.startsWith(`${prefix}/`);
}

function requireArray(value, label) {
  if (!Array.isArray(value)) throw new Error(`${label} must be an array`);
  return value;
}

function primaryGroupByPackage(primary) {
  const result = new Map();
  for (const [group, packages] of Object.entries(primary ?? {})) {
    for (const packageName of requireArray(packages, `primary group ${group}`)) {
      if (result.has(packageName)) {
        throw new Error(`package has multiple primary groups: ${packageName}`);
      }
      result.set(packageName, group);
    }
  }
  return result;
}

function workspacePackages(metadata, repoRoot, packageGroups) {
  const members = new Set(requireArray(metadata.workspace_members, "workspace_members"));
  const packageRecords = requireArray(metadata.packages, "packages");
  const recordsById = new Map(packageRecords.map((candidate) => [candidate.id, candidate]));
  const packageToGroup = primaryGroupByPackage(packageGroups.primary);
  const seenNames = new Set();

  const packages = [...members].map((memberId) => {
    const candidate = recordsById.get(memberId);
    if (!candidate) throw new Error(`workspace member is absent from metadata: ${memberId}`);
    if (seenNames.has(candidate.name)) {
      throw new Error(`duplicate workspace package name: ${candidate.name}`);
    }
    seenNames.add(candidate.name);

    const packageRoot = path.dirname(candidate.manifest_path);
    const relativeRoot = asRepoPath(path.relative(repoRoot, packageRoot));
    if (relativeRoot === ".." || relativeRoot.startsWith("../")) {
      throw new Error(`workspace package escapes repository: ${candidate.name}`);
    }
    const primaryGroup = packageToGroup.get(candidate.name);
    if (!primaryGroup) {
      throw new Error(`workspace package has no primary group: ${candidate.name}`);
    }
    return {
      id: candidate.id,
      name: candidate.name,
      root: relativeRoot === "" ? "." : relativeRoot,
      absoluteRoot: path.resolve(packageRoot),
      dependencies: requireArray(candidate.dependencies ?? [], `dependencies for ${candidate.name}`),
      primaryGroup,
    };
  });

  return packages.sort((left, right) => left.name.localeCompare(right.name));
}

function findPackage(packages, changedPath) {
  return packages
    .filter((candidate) => candidate.root !== "." && under(changedPath, candidate.root))
    .sort((left, right) => right.root.length - left.root.length)[0];
}

function findPathException(rules, changedPath) {
  return [...rules]
    .filter((candidate) => {
      if (candidate.path) return changedPath === candidate.path;
      return candidate.prefix && under(changedPath, candidate.prefix);
    })
    .sort((left, right) => {
      const leftMatch = left.path ?? left.prefix;
      const rightMatch = right.path ?? right.prefix;
      return rightMatch.length - leftMatch.length || leftMatch.localeCompare(rightMatch);
    })[0];
}

function reverseEdges(metadata, packages) {
  const packagesById = new Map(packages.map((candidate) => [candidate.id, candidate]));
  const packagesByRoot = new Map(
    packages.map((candidate) => [candidate.absoluteRoot, candidate]),
  );
  const reverse = new Map(packages.map((candidate) => [candidate.name, new Set()]));
  const nodes = requireArray(metadata.resolve?.nodes, "resolve.nodes");
  const nodesById = new Map(nodes.map((node) => [node.id, node]));

  for (const dependent of packages) {
    const node = nodesById.get(dependent.id);
    if (!node) throw new Error(`workspace package has no resolve node: ${dependent.name}`);

    // Resolved edges cover aliases, patches, and workspace-inherited declarations.
    for (const dependency of node.deps ?? []) {
      const dependencyPackage = packagesById.get(dependency.pkg);
      if (dependencyPackage) reverse.get(dependencyPackage.name).add(dependent.name);
    }
    for (const dependencyId of node.dependencies ?? []) {
      const dependencyPackage = packagesById.get(dependencyId);
      if (dependencyPackage) reverse.get(dependencyPackage.name).add(dependent.name);
    }

    // Declared local edges are unioned in as a conservative guard for optional and
    // target-specific dependencies absent from the current resolve feature set.
    for (const dependency of dependent.dependencies) {
      if (!dependency.path) continue;
      const dependencyPackage = packagesByRoot.get(path.resolve(dependency.path));
      if (dependencyPackage) reverse.get(dependencyPackage.name).add(dependent.name);
    }
  }
  return reverse;
}

function closureOf(start, reverse) {
  const closure = new Set(start);
  const queue = [...start].sort();
  while (queue.length > 0) {
    const current = queue.shift();
    for (const dependent of [...(reverse.get(current) ?? [])].sort()) {
      if (closure.has(dependent)) continue;
      closure.add(dependent);
      queue.push(dependent);
    }
  }
  return [...closure].sort();
}

function difference(left, right) {
  const rightSet = new Set(right);
  return left.filter((value) => !rightSet.has(value));
}

function exceptionRecord(changedPath, exception) {
  return {
    path: changedPath,
    class: exception.kind,
    rule: exception.path ?? exception.prefix,
    primary_groups: [...new Set(exception.primary_groups ?? [])].sort(),
    effects: [...new Set(exception.effects ?? [])].sort(),
    authorizes_legacy_reduction: exception.authorizes_legacy_reduction === true,
    evidence: exception.evidence,
  };
}

function validatePathExceptions(pathExceptions, packageGroups) {
  if (pathExceptions.schema !== "native-platform.ci-path-exceptions.v2") {
    throw new Error("CI path exceptions require schema v2");
  }
  const knownGroups = new Set(Object.keys(packageGroups.primary ?? {}));
  const matches = new Set();
  for (const exception of requireArray(pathExceptions.rules, "path exception rules")) {
    if (Boolean(exception.path) === Boolean(exception.prefix)) {
      throw new Error("path exception must declare exactly one path or prefix");
    }
    const match = exception.path ?? exception.prefix;
    if (matches.has(match)) throw new Error(`duplicate path exception: ${match}`);
    matches.add(match);
    if (!exception.kind || !exception.evidence) {
      throw new Error(`path exception lacks kind or evidence: ${match}`);
    }
    for (const group of exception.primary_groups ?? []) {
      if (!knownGroups.has(group)) {
        throw new Error(`path exception names unknown primary group: ${group}`);
      }
    }
    for (const effect of requireArray(exception.effects, `effects for ${match}`)) {
      if (!KNOWN_EFFECTS.has(effect)) {
        throw new Error(`path exception names unknown effect: ${effect}`);
      }
    }
  }
}

export function computeMetadataSelection({
  metadata,
  repoRoot,
  changed,
  packageGroups,
  pathExceptions,
}) {
  validatePathExceptions(pathExceptions, packageGroups);
  const packages = workspacePackages(metadata, repoRoot, packageGroups);
  const changedPackages = new Set();
  const exceptionGroups = new Set();
  const effects = new Set();
  const fileClassifications = [];
  const unknownPaths = [];

  for (const changedPath of changed) {
    const packageMatch = findPackage(packages, changedPath);
    if (packageMatch) {
      changedPackages.add(packageMatch.name);
      fileClassifications.push({
        path: changedPath,
        class: "workspace-package",
        package: packageMatch.name,
        package_root: packageMatch.root,
        primary_group: packageMatch.primaryGroup,
      });
      continue;
    }

    const exception = findPathException(pathExceptions.rules ?? [], changedPath);
    if (exception) {
      const record = exceptionRecord(changedPath, exception);
      fileClassifications.push(record);
      for (const group of record.primary_groups) exceptionGroups.add(group);
      for (const effect of record.effects) effects.add(effect);
      continue;
    }

    unknownPaths.push(changedPath);
    fileClassifications.push({ path: changedPath, class: "unknown" });
  }

  const forceFull = unknownPaths.length > 0 || effects.has("force_full");
  const reverse = reverseEdges(metadata, packages);
  const closure = forceFull
    ? packages.map((candidate) => candidate.name)
    : closureOf(changedPackages, reverse);
  const groupByPackage = new Map(
    packages.map((candidate) => [candidate.name, candidate.primaryGroup]),
  );
  const primaryGroups = new Set(exceptionGroups);
  for (const packageName of closure) primaryGroups.add(groupByPackage.get(packageName));

  return {
    schema: SELECTION_SCHEMA,
    metadata_status: "available",
    selection_applied: true,
    fallback: forceFull ? "full" : "none",
    fallback_reasons: [
      ...(unknownPaths.length > 0 ? ["unknown_path"] : []),
      ...(effects.has("force_full") ? ["explicit_full_exception"] : []),
    ],
    workspace_packages: packages.map((candidate) => candidate.name),
    changed_packages: [...changedPackages].sort(),
    reverse_dependency_closure: closure,
    primary_groups: [...primaryGroups].sort(),
    effects: [...effects].sort(),
    file_classifications: fileClassifications,
    unknown_paths: unknownPaths,
    unknown_path_fallback: unknownPaths.length > 0 ? "full" : "not_needed",
  };
}

export function unavailableSelection(reason, packageGroups) {
  const packages = Object.values(packageGroups.primary ?? {}).flat().sort();
  return {
    schema: SELECTION_SCHEMA,
    metadata_status: "unavailable",
    selection_applied: true,
    fallback: "full",
    fallback_reasons: ["metadata_unavailable"],
    workspace_packages: packages,
    changed_packages: [],
    reverse_dependency_closure: packages,
    primary_groups: Object.keys(packageGroups.primary ?? {}).sort(),
    effects: ["force_full"],
    file_classifications: [],
    unknown_paths: [],
    unknown_path_fallback: "not_evaluated",
    reason,
  };
}

export function legacyEquivalenceReport({
  legacySurface,
  generatedSurface,
  finalSurface,
  reductionEvidence = [],
  fallbackReasons = [],
}) {
  const legacy = [...new Set(legacySurface)].sort();
  const generated = [...new Set(generatedSurface)].sort();
  const final = [...new Set(finalSurface)].sort();
  const missingFromGenerated = difference(legacy, generated);
  const extraInGenerated = difference(generated, legacy);
  return {
    schema: SHADOW_SCHEMA,
    mode: "legacy-shadow",
    selection_applied: false,
    legacy_surface: legacy,
    generated_surface: generated,
    final_surface: final,
    missing_from_generated: missingFromGenerated,
    extra_in_generated: extraInGenerated,
    matches_legacy:
      missingFromGenerated.length === 0 && extraInGenerated.length === 0,
    generated_is_at_least_as_conservative: missingFromGenerated.length === 0,
    reduction_evidence: reductionEvidence,
    conservative_fallback_reasons: [...new Set(fallbackReasons)].sort(),
  };
}

export function readCargoMetadata(repoRoot, metadataPath = process.env.CI_CARGO_METADATA_PATH) {
  if (metadataPath) {
    const resolved = path.resolve(repoRoot, metadataPath);
    return JSON.parse(fs.readFileSync(resolved, "utf8"));
  }

  const result = spawnSync(
    process.env.CARGO ?? "cargo",
    ["metadata", "--locked", "--format-version", "1"],
    { cwd: repoRoot, encoding: "utf8", maxBuffer: 64 * 1024 * 1024 },
  );
  if (result.status !== 0) {
    const detail = (result.stderr || result.error?.message || "cargo metadata failed")
      .trim()
      .split("\n")
      .at(-1);
    throw new Error(detail);
  }
  return JSON.parse(result.stdout);
}

// Compatibility exports for consumers that imported the original shadow helper.
export const computeReverseDependencyShadow = computeMetadataSelection;
export const unavailableShadow = unavailableSelection;
