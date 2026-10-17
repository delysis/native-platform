#!/usr/bin/env node
import { spawnSync } from 'node:child_process';
import { closeSync, fsyncSync, openSync, writeSync } from 'node:fs';
import { basename, resolve } from 'node:path';

function usage() {
  return 'usage: node products/mom/scripts/run-voice-check.mjs <new-log-path> -- <command> [args...]';
}

const separator = process.argv.indexOf('--', 2);
if (separator < 0 || separator === 2 || separator + 1 >= process.argv.length) {
  console.error(usage());
  process.exit(2);
}

const logPath = resolve(process.argv[2]);
const command = process.argv[separator + 1];
const args = process.argv.slice(separator + 2);
let logFd;
try {
  // wx makes the evidence path a single-use capability. Never truncate or append
  // to an earlier run, including when the child command would fail to start.
  logFd = openSync(logPath, 'wx', 0o600);
} catch (error) {
  console.error(
    error?.code === 'EEXIST'
      ? `refusing existing voice-check log: ${logPath}`
      : `cannot create voice-check log ${logPath}: ${error?.code ?? 'unknown error'}`,
  );
  process.exit(2);
}

const started = Date.now();
let exitCode = 1;
let terminal = {};
try {
  writeSync(logFd, `${JSON.stringify({ record: 'start', started_unix_ms: started })}\n`);
  // Both child streams point at the same open file description; no pipe or
  // environment dump is retained, even for a long-running real-model check.
  const result = spawnSync(command, args, {
    cwd: process.cwd(),
    env: process.env,
    shell: false,
    stdio: ['ignore', logFd, logFd],
  });
  exitCode = result.status ?? 1;
  terminal = {
    record: 'exit',
    exit_code: exitCode,
    signal: result.signal ?? null,
    duration_ms: Date.now() - started,
    spawn_error_code: result.error?.code ?? null,
  };
} catch (error) {
  terminal = {
    record: 'exit',
    exit_code: 1,
    signal: null,
    duration_ms: Date.now() - started,
    spawn_error_code: error?.code ?? 'unknown',
  };
} finally {
  try {
    writeSync(logFd, `\n${JSON.stringify(terminal)}\n`);
    fsyncSync(logFd);
  } finally {
    closeSync(logFd);
  }
}

if (exitCode !== 0) {
  console.error(`${basename(command)} exited ${exitCode}; retained ${logPath}`);
}
process.exitCode = exitCode;
