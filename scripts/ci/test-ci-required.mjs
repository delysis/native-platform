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

test("a focused Mom plan accepts skipped root and requires its selected lanes", () => {
  const momPlan = {
    ...docsPlan,
    risk: "behavior",
    presence: { mom: true, loom: false },
    jobs: ["policy", "mom-linux", "frontend", "platform-macos"],
  };
  const needs = {
    plan: { result: "success" },
    policy: { result: "success" },
    "root-linux": { result: "skipped" },
    "mom-linux": { result: "success" },
    frontend: { result: "success" },
    "platform-macos": { result: "success" },
  };
  assert.equal(run(momPlan, needs).status, 0);
  needs.frontend = { result: "skipped" };
  assert.notEqual(run(momPlan, needs).status, 0);
});

test("matrix-backed job IDs are consumed as one fail-closed aggregate result", () => {
  const matrixPlan = {
    ...docsPlan,
    risk: "behavior",
    presence: { mom: true, loom: false },
    jobs: ["policy", "root-linux", "mom-linux", "platform-macos"],
  };
  const needs = {
    plan: { result: "success" },
    policy: { result: "success" },
    "root-linux": { result: "success" },
    "mom-linux": { result: "success" },
    "platform-macos": { result: "success" },
  };
  assert.equal(run(matrixPlan, needs).status, 0);
  for (const job of ["root-linux", "mom-linux", "platform-macos"]) {
    const failed = structuredClone(needs);
    failed[job].result = "failure";
    assert.notEqual(run(matrixPlan, failed).status, 0, `${job} must fail closed`);
  }
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
