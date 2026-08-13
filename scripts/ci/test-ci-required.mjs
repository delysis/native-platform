#!/usr/bin/env node

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import path from "node:path";
import test from "node:test";

const aggregator = path.resolve(import.meta.dirname, "ci-required.mjs");

function run(plan, needs) {
  return spawnSync(process.execPath, [aggregator], {
    encoding: "utf8",
    env: {
      ...process.env,
      CI_PLAN_JSON: JSON.stringify(plan),
      CI_NEEDS_JSON: JSON.stringify(needs),
    },
  });
}

const docsPlan = {
  schema: "native-platform.ci-plan.v1",
  risk: "docs",
  flags: { full: false },
  presence: { mom: false, loom: false },
  jobs: ["policy"],
};

test("required successes and unneeded skips pass", () => {
  const result = run(docsPlan, {
    plan: { result: "success" },
    policy: { result: "success" },
    "root-linux": { result: "skipped" },
  });
  assert.equal(result.status, 0, result.stderr);
});

test("a required skipped, failed, or missing job fails", () => {
  for (const resultName of ["skipped", "failure", undefined]) {
    const needs = {
      plan: { result: "success" },
      policy: { result: "success" },
    };
    if (resultName !== undefined) needs["root-linux"] = { result: resultName };
    const result = run({ ...docsPlan, jobs: ["policy", "root-linux"] }, needs);
    assert.notEqual(result.status, 0, `unexpected pass for ${resultName}`);
  }
});

test("planner failure cannot be hidden by a stale-looking plan", () => {
  const result = run(docsPlan, {
    plan: { result: "failure" },
    policy: { result: "success" },
  });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /plan/i);
});

test("an unexpected observed failure fails the aggregate", () => {
  const result = run(docsPlan, {
    plan: { result: "success" },
    policy: { result: "success" },
    "root-linux": { result: "failure" },
  });
  assert.notEqual(result.status, 0);
});

test("malformed and internally incomplete full plans fail closed", () => {
  const malformed = run({}, { plan: { result: "success" } });
  assert.notEqual(malformed.status, 0);

  const incompleteFull = run(
    {
      ...docsPlan,
      risk: "dependency",
      flags: { full: true },
      jobs: ["policy"],
    },
    {
      plan: { result: "success" },
      policy: { result: "success" },
    },
  );
  assert.notEqual(incompleteFull.status, 0);
  assert.match(incompleteFull.stderr, /full/i);
});
