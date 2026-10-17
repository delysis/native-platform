#!/usr/bin/env node
// Start a packaged voice candidate in a detached process and verify its exact
// executable and effective data directory before a UI driver attaches.
import { createHash } from 'node:crypto';
import { closeSync, existsSync, openSync, readFileSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { spawn, spawnSync } from 'node:child_process';
import {
  parseStartOptions,
  readPrivateStoreKeyFile,
  resolveCandidateDataDirectory,
  runtimeReadyLogConfirmsPath,
} from './voice-candidate-launch-policy.mjs';

const [operation, ...args] = process.argv.slice(2);
const fail = message => { throw new Error(message); };
const hashFile = path => createHash('sha256').update(readFileSync(path)).digest('hex');
const writeJson = (path, value) => writeFileSync(path, `${JSON.stringify(value, null, 2)}\n`);

function readJson(path) {
  try { return JSON.parse(readFileSync(path, 'utf8')); }
  catch (error) { fail(`Cannot read JSON ${path}: ${error.message}`); }
}

function candidateExecutable(candidatePath) {
  const candidate = readJson(candidatePath);
  if (candidate.status !== 'built-candidate-not-device-qualified') fail('Candidate manifest is not a completed candidate');
  if (!/^[a-zA-Z0-9_-]+$/.test(candidate.data_namespace || '')) fail('Candidate has no safe compiled data namespace');
  const bundle = resolve(candidate.bundle || '');
  const relativeExecutable = 'Contents/MacOS/mom-llama-app';
  const record = candidate.files?.find(file => file.path === relativeExecutable);
  if (!record || !/^[a-f0-9]{64}$/.test(record.sha256 || '')) fail('Candidate manifest lacks the executable digest');
  const executable = join(bundle, relativeExecutable);
  if (!existsSync(executable) || hashFile(executable) !== record.sha256) fail('Candidate executable is missing or differs from its manifest');
  return { candidate, executable, executableSha256: record.sha256 };
}

function processIdentity(pid, executable) {
  const command = spawnSync('/bin/ps', ['-p', String(pid), '-o', 'command='], { encoding: 'utf8' });
  const started = spawnSync('/bin/ps', ['-p', String(pid), '-o', 'lstart='], { encoding: 'utf8' });
  if (command.status !== 0 || started.status !== 0) return null;
  const commandLine = command.stdout.trim();
  if (commandLine !== executable && !commandLine.startsWith(`${executable} `)) return null;
  return { command_line: commandLine, process_start: started.stdout.trim() };
}

function runtimeReady(logPath, expectedDataDir) {
  return runtimeReadyLogConfirmsPath(readFileSync(logPath, 'utf8'), expectedDataDir);
}

async function start(candidatePath, receiptPath) {
  if (!candidatePath || !receiptPath) {
    fail('Usage: launch-voice-candidate.mjs start <candidate.json> <new-launch-receipt.json> [--data-dir <isolated-directory>] [--store-key-file <private-key-file>]');
  }
  let options;
  try { options = parseStartOptions(args.slice(2)); }
  catch (error) { fail(`${error.message}. Usage: launch-voice-candidate.mjs start <candidate.json> <new-launch-receipt.json> [--data-dir <isolated-directory>] [--store-key-file <private-key-file>]`); }
  let storeKey;
  if (options.storeKeyFile) {
    try { storeKey = readPrivateStoreKeyFile(resolve(options.storeKeyFile)); }
    catch (error) { fail(error.message); }
  }
  const receipt = resolve(receiptPath);
  const logPath = `${receipt.replace(/\.json$/i, '')}.stdout.log`;
  if (existsSync(receipt) || existsSync(logPath)) fail('Preserve prior evidence: choose new receipt and log paths');
  const { candidate, executable, executableSha256 } = candidateExecutable(resolve(candidatePath));
  // Honor an explicitly prepared isolated store first. An absent override uses
  // the compiled namespace, never the ordinary installed-app default store.
  const explicitPath = options.dataDirectory
    ? options.dataDirectory
    : process.env.LLAMA_NATIVE_KIT_DATA_DIR || null;
  const dataDirectory = resolveCandidateDataDirectory({
    home: homedir(), namespace: candidate.data_namespace, explicitPath,
  });
  const expectedDataDir = dataDirectory.path;
  const startedAt = new Date().toISOString();
  const evidence = {
    status: 'starting', candidate_manifest: resolve(candidatePath), candidate_source: candidate.source,
    bundle: resolve(candidate.bundle), executable, executable_sha256: executableSha256,
    data_namespace: candidate.data_namespace, data_directory_source: dataDirectory.source,
    effective_data_directory: expectedDataDir,
    started_at: startedAt, stdout_log: logPath,
  };
  let receiptFd;
  try { receiptFd = openSync(receipt, 'wx', 0o600); }
  catch { fail('Cannot exclusively create launch receipt; preserve existing evidence'); }
  closeSync(receiptFd);
  writeJson(receipt, evidence);
  let logFd;
  try { logFd = openSync(logPath, 'wx', 0o600); }
  catch { evidence.status = 'failed'; evidence.error = 'Cannot exclusively create launch log'; writeJson(receipt, evidence); fail(evidence.error); }

  const childEnvironment = { ...process.env, LLAMA_NATIVE_KIT_DATA_DIR: expectedDataDir };
  if (storeKey !== undefined) childEnvironment.LLAMA_NATIVE_KIT_STORE_KEY_HEX = storeKey;
  const child = spawn(executable, [], {
    cwd: dirname(executable), detached: true,
    env: childEnvironment,
    stdio: ['ignore', logFd, logFd],
  });
  closeSync(logFd);
  child.unref();
  evidence.pid = child.pid;
  writeJson(receipt, evidence);
  if (!child.pid) {
    evidence.status = 'failed'; evidence.error = 'Operating system did not return a child PID'; writeJson(receipt, evidence); fail(evidence.error);
  }

  const deadline = Date.now() + 120_000;
  while (Date.now() < deadline) {
    const identity = processIdentity(child.pid, executable);
    if (identity && runtimeReady(logPath, expectedDataDir)) {
      evidence.status = 'ready';
      evidence.verified_at = new Date().toISOString();
      Object.assign(evidence, identity);
      writeJson(receipt, evidence);
      console.log(JSON.stringify({ status: evidence.status, pid: evidence.pid, executable, effective_data_directory: expectedDataDir, receipt }, null, 2));
      return;
    }
    if (!identity) break;
    await new Promise(resolvePromise => setTimeout(resolvePromise, 250));
  }
  // Only stop the exact candidate process we launched. A vanished/reused PID
  // must never turn timeout cleanup into a signal to an unrelated process.
  if (processIdentity(child.pid, executable)) {
    try { process.kill(child.pid, 'SIGTERM'); } catch {}
  }
  evidence.status = 'failed';
  evidence.error = 'Candidate did not remain alive and report the expected runtime data directory within 120 seconds';
  writeJson(receipt, evidence);
  fail(`${evidence.error}; preserve ${receipt} and ${logPath}`);
}

function verify(receiptPath) {
  if (!receiptPath || args.length !== 1) fail('Usage: launch-voice-candidate.mjs verify <launch-receipt.json>');
  const path = resolve(receiptPath);
  const evidence = readJson(path);
  if (evidence.status !== 'ready' || !Number.isInteger(evidence.pid)) fail('Launch receipt is not verified-ready');
  const { candidate, executable, executableSha256 } = candidateExecutable(evidence.candidate_manifest);
  if (candidate.source?.sha !== evidence.candidate_source?.sha || executable !== evidence.executable
      || executableSha256 !== evidence.executable_sha256) fail('Candidate identity no longer matches the ready receipt');
  const expectedDataDirectory = resolveCandidateDataDirectory({
    home: homedir(), namespace: candidate.data_namespace,
    explicitPath: evidence.data_directory_source === 'explicit-isolated-directory'
      ? evidence.effective_data_directory : null,
  });
  if (expectedDataDirectory.path !== evidence.effective_data_directory
      || expectedDataDirectory.source !== evidence.data_directory_source) {
    fail('Receipt data directory does not match the candidate isolation policy');
  }
  const identity = processIdentity(evidence.pid, executable);
  if (!identity || identity.process_start !== evidence.process_start) fail('Candidate PID is absent or has been reused');
  if (!runtimeReady(evidence.stdout_log, evidence.effective_data_directory)) fail('Startup log does not prove the isolated runtime data directory');
  console.log(JSON.stringify({ status: 'verified-ready', pid: evidence.pid, executable, effective_data_directory: evidence.effective_data_directory }, null, 2));
}

try {
  if (operation === 'start') await start(args[0], args[1]);
  else if (operation === 'verify') verify(args[0]);
  else fail('Usage: launch-voice-candidate.mjs <start|verify> ...');
} catch (error) {
  console.error(error.message);
  process.exitCode = 1;
}
