import { spawnSync } from "node:child_process";
import path from "node:path";

export const SHADOW_SCHEMA = "native-platform.ci-reverse-closure-shadow.v1";

function asRepoPath(value) {
  return value.split(path.sep).join("/").replace(/^\.\//, "");
}

function under(candidate, prefix) {
  return candidate === prefix || candidate.startsWith(`${prefix}/`);
}

function workspacePackages(metadata, repoRoot) {
  const members = new Set(metadata.workspace_members ?? []);
  return (metadata.packages ?? [])
    .filter((candidate) => members.has(candidate.id))
    .map((candidate) => {
      const packageRoot = path.dirname(candidate.manifest_path);
      const relativeRoot = asRepoPath(path.relative(repoRoot, packageRoot));
      if (relativeRoot === ".." || relativeRoot.startsWith("../")) {
        throw new Error(`workspace package escapes repository: ${candidate.name}`);
      }
      return {
        id: candidate.id,
        name: candidate.name,
        root: relativeRoot === "" ? "." : relativeRoot,
        absoluteRoot: path.resolve(packageRoot),
        dependencies: candidate.dependencies ?? [],
      };
    })
    .sort((left, right) => left.name.localeCompare(right.name));
}

function primaryGroupByPackage(primary) {
  const result = new Map();
  for (const [group, packages] of Object.entries(primary)) {
    for (const packageName of packages) {
      if (result.has(packageName)) {
        throw new Error(`package has multiple primary groups: ${packageName}`);
      }
      result.set(packageName, group);
    }
  }
  return result;
}

function findPackage(packages, changedPath) {
  return packages
    .filter((candidate) => candidate.root !== "." && under(changedPath, candidate.root))
    .sort((left, right) => right.root.length - left.root.length)[0];
}

function findAssetGroup(assetGroups, changedPath) {
  return [...assetGroups]
    .filter((candidate) => under(changedPath, candidate.prefix))
    .sort((left, right) => right.prefix.length - left.prefix.length)[0];
}

function findPathException(exceptions, changedPath) {
  return exceptions.find((candidate) => {
    if (candidate.path) return changedPath === candidate.path;
    return candidate.prefix && under(changedPath, candidate.prefix);
  });
}

function reverseEdges(packages) {
  const byRoot = new Map(packages.map((candidate) => [candidate.absoluteRoot, candidate.name]));
  const reverse = new Map(packages.map((candidate) => [candidate.name, new Set()]));
  for (const dependent of packages) {
    for (const dependency of dependent.dependencies) {
      if (!dependency.path) continue;
      const dependencyName = byRoot.get(path.resolve(dependency.path));
      if (dependencyName) reverse.get(dependencyName).add(dependent.name);
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

export function computeReverseDependencyShadow({
  metadata,
  repoRoot,
  changed,
  packageGroups,
  pathExceptions,
  authoritativePrimaryGroups,
}) {
  const packages = workspacePackages(metadata, repoRoot);
  const packageToGroup = primaryGroupByPackage(packageGroups.primary);
  const changedPackages = new Set();
  const assetGroups = new Set();
  const exceptions = [];
  const unmapped = [];

  for (const changedPath of changed) {
    const packageMatch = findPackage(packages, changedPath);
    if (packageMatch) {
      changedPackages.add(packageMatch.name);
      continue;
    }
    const assetMatch = findAssetGroup(pathExceptions.asset_groups ?? [], changedPath);
    if (assetMatch) {
      assetGroups.add(assetMatch.primary_group);
      exceptions.push({ path: changedPath, kind: "asset", rule: assetMatch.prefix });
      continue;
    }
    const exception = findPathException(pathExceptions.authoritative_exceptions ?? [], changedPath);
    if (exception) {
      exceptions.push({
        path: changedPath,
        kind: exception.kind,
        rule: exception.path ?? exception.prefix,
      });
      continue;
    }
    unmapped.push(changedPath);
  }

  const forceFull = unmapped.length > 0;
  const closure = forceFull
    ? packages.map((candidate) => candidate.name)
    : closureOf(changedPackages, reverseEdges(packages));
  const groups = new Set(assetGroups);
  for (const packageName of closure) {
    const group = packageToGroup.get(packageName);
    if (!group) throw new Error(`workspace package has no primary group: ${packageName}`);
    groups.add(group);
  }
  const shadowGroups = [...groups].sort();
  const authoritative = [...new Set(authoritativePrimaryGroups)].sort();

  return {
    schema: SHADOW_SCHEMA,
    mode: "shadow",
    metadata_status: "available",
    selection_applied: false,
    promotion_allowed: false,
    promotion_prohibition:
      "Reverse-closure output is observational only until representative shadow equivalence is reviewed and explicitly promoted.",
    changed_packages: [...changedPackages].sort(),
    reverse_dependency_closure: closure,
    primary_groups: shadowGroups,
    authoritative_primary_groups: authoritative,
    missing_from_authoritative: difference(shadowGroups, authoritative),
    extra_in_authoritative: difference(authoritative, shadowGroups),
    matches_authoritative:
      difference(shadowGroups, authoritative).length === 0 &&
      difference(authoritative, shadowGroups).length === 0,
    explicit_path_exceptions: exceptions,
    unknown_paths: unmapped,
    unknown_path_fallback: forceFull ? "full" : "not_needed",
  };
}

export function unavailableShadow(reason) {
  return {
    schema: SHADOW_SCHEMA,
    mode: "shadow",
    metadata_status: "unavailable",
    selection_applied: false,
    promotion_allowed: false,
    promotion_prohibition:
      "Reverse-closure output is unavailable and cannot alter authoritative selection.",
    reason,
  };
}

export function readCargoMetadata(repoRoot) {
  const result = spawnSync(
    process.env.CARGO ?? "cargo",
    ["metadata", "--locked", "--no-deps", "--format-version", "1"],
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
