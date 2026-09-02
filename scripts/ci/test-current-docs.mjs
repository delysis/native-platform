import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";

import { validateCurrentDocs } from "./validate-current-docs.mjs";

function git(repo, ...args) {
  const result = spawnSync("git", args, { cwd: repo, encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
  return result.stdout.trim();
}

function fixture() {
  const repo = fs.mkdtempSync(path.join(os.tmpdir(), "delysis-current-docs-"));
  git(repo, "init", "--quiet");
  git(repo, "config", "user.email", "tests@delysis.invalid");
  git(repo, "config", "user.name", "Delysis Tests");
  fs.mkdirSync(path.join(repo, "current/service"), { recursive: true });
  fs.writeFileSync(path.join(repo, "current/service/Cargo.toml"), "[package]\nname='fixture'\n");
  fs.mkdirSync(path.join(repo, "retired/plugin"), { recursive: true });
  fs.writeFileSync(path.join(repo, "retired/plugin/Cargo.toml"), "[package]\nname='retired'\n");
  git(repo, "add", ".");
  git(repo, "commit", "--quiet", "-m", "historical edge");
  const parent = git(repo, "rev-parse", "HEAD");
  fs.rmSync(path.join(repo, "retired"), { recursive: true });
  fs.mkdirSync(path.join(repo, "docs"), { recursive: true });
  const document = [
    "<!-- current-service-surface: fixture -->",
    `<!-- retired-edge-parent: ${parent} -->`,
    "current/service",
    "retired/plugin",
    "",
  ].join("\n");
  fs.writeFileSync(path.join(repo, "docs/surface.md"), document);
  const manifest = {
    schema: "delysis.current-service-surfaces.v1",
    surfaces: [{
      id: "fixture",
      documents: ["docs/surface.md"],
      current_paths: ["current/service"],
      retired_edges: [{ path: "retired/plugin", last_present_commit: parent }],
    }],
  };
  const manifestPath = path.join(repo, "manifest.json");
  fs.writeFileSync(manifestPath, JSON.stringify(manifest));
  return { repo, manifest, manifestPath };
}

test("accepts exact current paths and a historical retired edge", () => {
  const { repo, manifestPath } = fixture();
  assert.deepEqual(validateCurrentDocs({ repoRoot: repo, manifestPath }), {
    schema: "delysis.current-service-surfaces.v1",
    surfaces: 1,
    documents: 1,
    current_paths: 1,
    retired_edges: 1,
  });
});

test("rejects a missing current path", () => {
  const { repo, manifest, manifestPath } = fixture();
  manifest.surfaces[0].current_paths = ["current/missing"];
  fs.writeFileSync(manifestPath, JSON.stringify(manifest));
  assert.throws(
    () => validateCurrentDocs({ repoRoot: repo, manifestPath }),
    /current path.*does not exist|current_paths\[0\].*does not exist/,
  );
});

test("rejects a retired edge restored into the current tree", () => {
  const { repo, manifestPath } = fixture();
  fs.mkdirSync(path.join(repo, "retired/plugin"), { recursive: true });
  assert.throws(
    () => validateCurrentDocs({ repoRoot: repo, manifestPath }),
    /retired edge exists in the current tree/,
  );
});

test("rejects documentation without the exact historical parent marker", () => {
  const { repo, manifestPath } = fixture();
  fs.writeFileSync(path.join(repo, "docs/surface.md"), "retired/plugin\n");
  assert.throws(
    () => validateCurrentDocs({ repoRoot: repo, manifestPath }),
    /missing <!-- current-service-surface: fixture -->/,
  );
});
