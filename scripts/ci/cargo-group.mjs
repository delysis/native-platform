#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";

const root = path.resolve(import.meta.dirname, "../..");
const groups = JSON.parse(
  fs.readFileSync(path.join(root, "ci/package-groups.json"), "utf8"),
);
const [command, ...requested] = process.argv.slice(2);

if (!["check", "clippy", "fmt", "test"].includes(command) || requested.length === 0) {
  throw new Error(
    "usage: cargo-group.mjs <check|clippy|fmt|test> <group> [group ...]",
  );
}

const known = { ...groups.primary, ...groups.secondary };
const packages = [
  ...new Set(
    requested.flatMap((group) => {
      if (!known[group]) throw new Error(`unknown package group: ${group}`);
      return known[group];
    }),
  ),
].sort();

const args = [command];
if (command !== "fmt") args.push("--locked");
for (const packageName of packages) args.push("--package", packageName);
if (command !== "fmt") args.push("--all-targets");
if (command === "clippy") args.push("--", "-D", "warnings");
else if (command === "fmt") args.push("--", "--check");

const result = spawnSync(process.env.CARGO ?? "cargo", args, {
  cwd: root,
  stdio: "inherit",
});
if (result.error) throw result.error;
process.exit(result.status ?? 1);
