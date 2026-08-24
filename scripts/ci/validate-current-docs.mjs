#!/usr/bin/env node

import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const SCHEMA = "delysis.current-service-surfaces.v1";
const SHA_1 = /^[0-9a-f]{40}$/;

function fail(message) {
  throw new Error(`current service documentation is invalid: ${message}`);
}

function checkedRelativePath(value, field) {
  if (typeof value !== "string" || value.length === 0) {
    fail(`${field} must be a non-empty repository-relative path`);
  }
  if (
    path.isAbsolute(value) ||
    value.includes("\\") ||
    value === "." ||
    value === ".." ||
    value.startsWith("../") ||
    path.posix.normalize(value) !== value
  ) {
    fail(`${field} is not a canonical repository-relative path: ${value}`);
  }
  return value;
}

function lexicalPath(repoRoot, relative, field) {
  const checked = checkedRelativePath(relative, field);
  const resolved = path.resolve(repoRoot, checked);
  const prefix = `${path.resolve(repoRoot)}${path.sep}`;
  if (!resolved.startsWith(prefix)) {
    fail(`${field} escapes the repository: ${relative}`);
  }
  return resolved;
}

function requireCurrentPath(repoRoot, relative, field) {
  const resolved = lexicalPath(repoRoot, relative, field);
  let metadata;
  try {
    metadata = fs.lstatSync(resolved);
  } catch (error) {
    if (error?.code === "ENOENT") {
      fail(`${field} does not exist: ${relative}`);
    }
    throw error;
  }
  if (metadata.isSymbolicLink()) {
    fail(`${field} must not be a symbolic link: ${relative}`);
  }
  const canonical = fs.realpathSync(resolved);
  const root = `${fs.realpathSync(repoRoot)}${path.sep}`;
  if (!canonical.startsWith(root)) {
    fail(`${field} resolves outside the repository: ${relative}`);
  }
  return resolved;
}

function gitObjectExists(repoRoot, commit, relative) {
  const result = spawnSync("git", ["cat-file", "-e", `${commit}:${relative}`], {
    cwd: repoRoot,
    encoding: "utf8",
    timeout: 10_000,
  });
  if (result.error) {
    fail(`could not inspect historical Git object ${commit}:${relative}: ${result.error.message}`);
  }
  return result.status === 0;
}

export function validateCurrentDocs({
  repoRoot,
  manifestPath = path.join(repoRoot, "docs/current-service-surfaces.json"),
}) {
  const manifest = JSON.parse(fs.readFileSync(manifestPath, "utf8"));
  if (manifest.schema !== SCHEMA || !Array.isArray(manifest.surfaces)) {
    fail(`manifest must use ${SCHEMA}`);
  }

  const ids = new Set();
  let documentCount = 0;
  let currentPathCount = 0;
  let retiredEdgeCount = 0;
  for (const [surfaceIndex, surface] of manifest.surfaces.entries()) {
    const prefix = `surfaces[${surfaceIndex}]`;
    if (typeof surface.id !== "string" || !/^[a-z][a-z0-9-]*$/.test(surface.id)) {
      fail(`${prefix}.id must be a stable lowercase identifier`);
    }
    if (ids.has(surface.id)) {
      fail(`duplicate surface id: ${surface.id}`);
    }
    ids.add(surface.id);
    if (
      !Array.isArray(surface.documents) ||
      surface.documents.length === 0 ||
      !Array.isArray(surface.current_paths) ||
      surface.current_paths.length === 0 ||
      !Array.isArray(surface.retired_edges) ||
      surface.retired_edges.length === 0
    ) {
      fail(`${prefix} must declare documents, current_paths, and retired_edges`);
    }

    const retired = surface.retired_edges.map((edge, edgeIndex) => {
      const field = `${prefix}.retired_edges[${edgeIndex}]`;
      const relative = checkedRelativePath(edge.path, `${field}.path`);
      if (!SHA_1.test(edge.last_present_commit ?? "")) {
        fail(`${field}.last_present_commit must be one exact 40-character Git SHA-1`);
      }
      const livePath = lexicalPath(repoRoot, relative, `${field}.path`);
      if (fs.existsSync(livePath)) {
        fail(`retired edge exists in the current tree: ${relative}`);
      }
      if (!gitObjectExists(repoRoot, edge.last_present_commit, relative)) {
        fail(`retired edge is absent from declared historical parent: ${edge.last_present_commit}:${relative}`);
      }
      retiredEdgeCount += 1;
      return edge;
    });

    for (const [pathIndex, relative] of surface.current_paths.entries()) {
      requireCurrentPath(repoRoot, relative, `${prefix}.current_paths[${pathIndex}]`);
      currentPathCount += 1;
    }

    for (const [documentIndex, relative] of surface.documents.entries()) {
      const field = `${prefix}.documents[${documentIndex}]`;
      const documentPath = requireCurrentPath(repoRoot, relative, field);
      const text = fs.readFileSync(documentPath, "utf8");
      const marker = `<!-- current-service-surface: ${surface.id} -->`;
      if (!text.includes(marker)) {
        fail(`${relative} is missing ${marker}`);
      }
      for (const edge of retired) {
        const parentMarker = `<!-- retired-edge-parent: ${edge.last_present_commit} -->`;
        if (!text.includes(parentMarker) || !text.includes(edge.path)) {
          fail(`${relative} must identify retired edge ${edge.path} and its exact parent`);
        }
      }
      documentCount += 1;
    }
  }

  return {
    schema: SCHEMA,
    surfaces: ids.size,
    documents: documentCount,
    current_paths: currentPathCount,
    retired_edges: retiredEdgeCount,
  };
}

const thisFile = fileURLToPath(import.meta.url);
if (process.argv[1] && path.resolve(process.argv[1]) === thisFile) {
  const repoRoot = path.resolve(path.dirname(thisFile), "../..");
  const result = validateCurrentDocs({ repoRoot });
  process.stdout.write(`${JSON.stringify(result)}\n`);
}
