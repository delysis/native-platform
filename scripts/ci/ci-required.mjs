#!/usr/bin/env node

function parseJsonEnv(name) {
  const raw = process.env[name];
  if (!raw) throw new Error(`missing ${name}`);
  try {
    return JSON.parse(raw);
  } catch (error) {
    throw new Error(`invalid ${name}: ${error.message}`);
  }
}

const plan = parseJsonEnv("CI_PLAN_JSON");
const needs = parseJsonEnv("CI_NEEDS_JSON");

const knownJobs = [
  "policy",
  "root-linux",
  "native-linux",
  "gateway-linux",
  "attachment-linux",
  "information-linux",
  "speech-linux",
  "mom-linux",
  "loom-linux",
  "frontend",
  "platform-macos",
  "dependency-graph",
  "import-history",
  "fuzz-build",
];

if (plan.schema !== "native-platform.ci-plan.v1") {
  throw new Error("missing or invalid CI plan schema");
}
if (!Array.isArray(plan.jobs) || plan.jobs.length === 0) {
  throw new Error("CI plan jobs must be a non-empty array");
}
if (!plan.jobs.every((job) => typeof job === "string" && knownJobs.includes(job))) {
  throw new Error("CI plan contains an unknown job");
}
if (new Set(plan.jobs).size !== plan.jobs.length) {
  throw new Error("CI plan contains duplicate jobs");
}
if (!plan.jobs.includes("policy")) {
  throw new Error("CI plan must always require policy");
}

const failures = [];
if (needs.plan?.result !== "success") {
  failures.push(`plan: expected success, observed ${needs.plan?.result ?? "missing"}`);
}

if (plan.flags?.full === true) {
  const fullJobs = [
    "policy",
    "root-linux",
    "native-linux",
    "gateway-linux",
    "attachment-linux",
    "information-linux",
    "speech-linux",
    "frontend",
    "platform-macos",
    "dependency-graph",
    "import-history",
    "fuzz-build",
  ];
  if (plan.presence?.mom === true) fullJobs.push("mom-linux");
  if (plan.presence?.loom === true) fullJobs.push("loom-linux");
  for (const job of fullJobs) {
    if (!plan.jobs.includes(job)) failures.push(`full plan omitted ${job}`);
  }
}

for (const expected of plan.jobs) {
  const result = needs[expected]?.result;
  if (result !== "success") {
    failures.push(`${expected}: expected success, observed ${result ?? "missing"}`);
  }
}

for (const [job, value] of Object.entries(needs)) {
  if (job === "plan" || job === "ci-required") continue;
  const result = value?.result;
  if (result && result !== "success" && result !== "skipped") {
    failures.push(`${job}: ${result}`);
  }
}

if (failures.length > 0) {
  console.error("CI required gate failed:");
  for (const failure of [...new Set(failures)]) console.error(`- ${failure}`);
  process.exit(1);
}

console.log(`ci-required passed for risk=${plan.risk}`);
console.log(`required jobs: ${plan.jobs.join(", ")}`);
