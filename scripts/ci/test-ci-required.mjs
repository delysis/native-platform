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
    flags: { full: false, mom: true },
    presence: { mom: true, loom: false },
    jobs: ["policy", "mom-linux", "mom-windows", "frontend", "platform-macos"],
  };
  const needs = {
    plan: { result: "success" },
    policy: { result: "success" },
    "root-linux": { result: "skipped" },
    "mom-linux": { result: "success" },
    "mom-windows": { result: "success" },
    frontend: { result: "success" },
    "platform-macos": { result: "success" },
  };
  assert.equal(run(momPlan, needs).status, 0);
  needs.frontend = { result: "skipped" };
  assert.notEqual(run(momPlan, needs).status, 0);
});

test("a Mom plan keeps Windows coverage scheduled without gating on its result", () => {
  const momPlan = {
    ...docsPlan,
    risk: "behavior",
    flags: { full: false, mom: true },
    presence: { mom: true, loom: false },
    jobs: ["policy", "mom-linux", "mom-windows"],
  };
  const needs = {
    plan: { result: "success" },
    policy: { result: "success" },
    "mom-linux": { result: "success" },
    "mom-windows": { result: "success" },
  };
  assert.equal(run(momPlan, needs).status, 0);

  const skippedWindows = structuredClone(needs);
  skippedWindows["mom-windows"].result = "skipped";
  assert.equal(run(momPlan, skippedWindows).status, 0);

  const omittedWindows = structuredClone(momPlan);
  omittedWindows.jobs = omittedWindows.jobs.filter((job) => job !== "mom-windows");
  assert.notEqual(run(omittedWindows, needs).status, 0);
});

test("a Loom frontend plan cannot omit or skip its macOS WebKit gate", () => {
  const loomPlan = {
    ...docsPlan,
    risk: "behavior",
    flags: { full: false, loom: true, frontend_loom: true },
    presence: { mom: false, loom: true },
    jobs: ["policy", "loom-linux", "loom-windows", "frontend", "platform-macos"],
    macos_matrix: ["release", "loom"],
  };
  const needs = {
    plan: { result: "success" },
    policy: { result: "success" },
    "loom-linux": { result: "success" },
    "loom-windows": { result: "success" },
    frontend: { result: "success" },
    "platform-macos": { result: "success" },
  };
  assert.equal(run(loomPlan, needs).status, 0);

  const skippedMac = structuredClone(needs);
  skippedMac["platform-macos"].result = "skipped";
  assert.notEqual(run(loomPlan, skippedMac).status, 0);

  const omittedJob = structuredClone(loomPlan);
  omittedJob.jobs = omittedJob.jobs.filter((job) => job !== "platform-macos");
  assert.notEqual(run(omittedJob, needs).status, 0);

  const omittedMatrixEntry = structuredClone(loomPlan);
  omittedMatrixEntry.macos_matrix = ["release"];
  assert.notEqual(run(omittedMatrixEntry, needs).status, 0);
});

test("a Loom plan keeps Windows coverage scheduled without gating on its result", () => {
  const loomPlan = {
    ...docsPlan,
    risk: "behavior",
    flags: { full: false, loom: true },
    presence: { mom: false, loom: true },
    jobs: ["policy", "loom-linux", "loom-windows"],
  };
  const needs = {
    plan: { result: "success" },
    policy: { result: "success" },
    "loom-linux": { result: "success" },
    "loom-windows": { result: "success" },
  };
  assert.equal(run(loomPlan, needs).status, 0);

  const skippedWindows = structuredClone(needs);
  skippedWindows["loom-windows"].result = "skipped";
  assert.equal(run(loomPlan, skippedWindows).status, 0);

  const omittedWindows = structuredClone(loomPlan);
  omittedWindows.jobs = omittedWindows.jobs.filter((job) => job !== "loom-windows");
  assert.notEqual(run(omittedWindows, needs).status, 0);
});

test("an Information plan keeps Windows coverage scheduled without gating on its result", () => {
  const informationPlan = {
    ...docsPlan,
    risk: "behavior",
    flags: { full: false, information: true },
    jobs: ["policy", "information-linux", "information-windows"],
  };
  const needs = {
    plan: { result: "success" },
    policy: { result: "success" },
    "information-linux": { result: "success" },
    "information-windows": { result: "success" },
  };
  assert.equal(run(informationPlan, needs).status, 0);

  const skippedWindows = structuredClone(needs);
  skippedWindows["information-windows"].result = "skipped";
  assert.equal(run(informationPlan, skippedWindows).status, 0);

  const omittedWindows = structuredClone(informationPlan);
  omittedWindows.jobs = omittedWindows.jobs.filter(
    (job) => job !== "information-windows",
  );
  assert.notEqual(run(omittedWindows, needs).status, 0);
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
    assert.equal(run(matrixPlan, failed).status === 0, job !== "platform-macos", job);
  }
});

test("selected cross-platform inventory runs outside the merge gate", () => {
  const ignoredPlan = {
    ...docsPlan,
    risk: "behavior",
    flags: { full: false, ignored_tests: true },
    jobs: ["policy", "ignored-tests"],
  };
  const needs = {
    plan: { result: "success" },
    policy: { result: "success" },
    "ignored-tests": { result: "success" },
  };
  assert.equal(run(ignoredPlan, needs).status, 0);
  needs["ignored-tests"] = { result: "skipped" };
  assert.equal(run(ignoredPlan, needs).status, 0);
});

test("a required skipped, failed, or missing job fails", () => {
  for (const resultName of ["skipped", "failure", undefined]) {
    const needs = {
      plan: { result: "success" },
      policy: { result: "success" },
    };
    if (resultName !== undefined) needs["platform-macos"] = { result: resultName };
    const result = run({ ...docsPlan, jobs: ["policy", "platform-macos"] }, needs);
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

test("an advisory platform failure does not block macOS development", () => {
  const result = run(docsPlan, {
    plan: { result: "success" },
    policy: { result: "success" },
    "root-linux": { result: "failure" },
  });
  assert.equal(result.status, 0);
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

test("the gate completes while selected advisory jobs are absent or still running", () => {
  const plan = { ...docsPlan, jobs: ["policy", "platform-macos", "root-linux", "ignored-tests"] };
  for (const advisory of [undefined, "failure", "cancelled", "pending"]) {
    const needs = { plan: { result: "success" }, policy: { result: "success" }, "platform-macos": { result: "success" } };
    if (advisory) needs["root-linux"] = { result: advisory };
    assert.equal(run(plan, needs).status, 0, advisory);
  }
});
