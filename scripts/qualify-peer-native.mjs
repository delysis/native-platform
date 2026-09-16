#!/usr/bin/env node
// Instrument an isolated exact-revision worktree, never the working checkout.
// Execute the actual production peer adapter through the plugin's libtest gate.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import crypto from "node:crypto";
import { spawn, spawnSync } from "node:child_process";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";

export const nativeTestName = "peer_compute::batch::native_qualification::independent_peers_share_native_decode";
export const decodeSite = `        context
            .decode(&mut batch)
            .map_err(|error| native_decode_error("failed to decode generation batch", error))?;`;
export const independentSite = "        let ticket = handle.generate_batch(native_request.clone())?;";

function instrument(source, site, marker, observation) {
  if (source.includes(marker) || source.split(site).length !== 2) {
    throw new Error("instrumentation no longer matches exactly one reviewed native site");
  }
  return source.replace(site, `${site}\n${observation}`);
}

export function instrumentDecodeSource(source) {
  return instrument(source, decodeSite, "LOOM_NATIVE_BATCH_TRACE", `        eprintln!("LOOM_NATIVE_BATCH_TRACE {}", serde_json::json!({
            "kind": "loom_native_decode_batch_v1",
            "request_id": request.request_id,
            "case_ids": next_tokens.iter().map(|(index, _)| branches[*index].request.branch_id.as_str()).collect::<Vec<_>>(),
            "sequence_ids": next_tokens.iter().map(|(index, _)| branches[*index].sequence_id).collect::<Vec<_>>(),
            "generated_counts": next_tokens.iter().map(|(index, _)| branches[*index].generated).collect::<Vec<_>>(),
        }));`);
}

export function instrumentIndependentSource(source) {
  return instrument(source, independentSite, "LOOM_NATIVE_BATCH_MAPPING", `        eprintln!("LOOM_NATIVE_BATCH_MAPPING {}", serde_json::json!({
            "kind": "loom_native_batch_mapping_v1",
            "request_id": native_request.request_id,
            "members": requests.iter().map(|item| serde_json::json!({
                "request_id": item.request_id,
                "case_id": item.case_id,
            })).collect::<Vec<_>>(),
        }));`);
}

function parseObservation(line, prefix, kind) {
  if (!line.startsWith(prefix)) return null;
  const record = JSON.parse(line.slice(prefix.length));
  if (record.kind !== kind) throw new Error("invalid native observation kind");
  return record;
}

export function parseTraceLine(line) {
  return parseObservation(line, "LOOM_NATIVE_BATCH_TRACE ", "loom_native_decode_batch_v1");
}

export function parseMappingLine(line) {
  return parseObservation(line, "LOOM_NATIVE_BATCH_MAPPING ", "loom_native_batch_mapping_v1");
}

export function parseTestExecutable(line) {
  let message;
  try { message = JSON.parse(line); } catch { return null; }
  return message.reason === "compiler-artifact"
    && message.target?.name === "tauri_plugin_loom"
    && message.target?.kind?.includes("lib")
    && message.profile?.test === true
    && typeof message.executable === "string"
    && message.executable.length > 0
    ? message.executable : null;
}

function git(cwd, args) {
  const result = spawnSync("git", args, { cwd, encoding: "utf8" });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`git ${args[0]} failed: ${result.stderr}`);
  return result.stdout.trim();
}

function sha256(bytes) {
  return crypto.createHash("sha256").update(bytes).digest("hex");
}

async function fileHash(filename) {
  const hash = crypto.createHash("sha256");
  for await (const chunk of fs.createReadStream(filename)) hash.update(chunk);
  return hash.digest("hex");
}

async function execute(command, args, { cwd, env, output, logName, onStdout, onStderr }) {
  const log = fs.openSync(path.join(output, logName), "wx", 0o600);
  let logBytes = 0;
  let failure = null;
  // A timed-out Cargo process must not leave its compiler children running.
  const grouped = process.platform !== "win32";
  const child = spawn(command, args, { cwd, env, detached: grouped, stdio: ["ignore", "pipe", "pipe"] });
  const signal = (name) => {
    try {
      if (grouped && child.pid) process.kill(-child.pid, name);
      else child.kill(name);
    } catch (error) {
      if (error.code !== "ESRCH") failure ??= error;
    }
  };
  let forceKill;
  const terminate = (error) => {
    failure ??= error;
    signal("SIGTERM");
    forceKill ??= setTimeout(() => signal("SIGKILL"), 5000);
    forceKill.unref();
  };
  const timer = setTimeout(() => terminate(new Error("qualification command exceeded 20 minutes")), 20 * 60 * 1000);
  const capture = (chunk) => {
    logBytes += chunk.length;
    if (logBytes > 64 * 1024 * 1024) {
      terminate(new Error("qualification command log exceeded 64 MiB"));
      return;
    }
    fs.writeSync(log, chunk);
  };
  child.stdout.on("data", capture);
  child.stderr.on("data", capture);
  const observe = (stream, callback) => {
    if (!callback) return null;
    const lines = createInterface({ input: stream });
    lines.on("line", (line) => {
      try { callback(line); } catch (error) { terminate(error); }
    });
    return lines;
  };
  const stdout = observe(child.stdout, onStdout);
  const stderr = observe(child.stderr, onStderr);
  try {
    const code = await new Promise((resolve, reject) => {
      child.once("error", reject);
      child.once("close", resolve);
    });
    if (failure) throw failure;
    if (code !== 0) throw new Error(`${command} failed (${code}); see ${logName}`);
  } finally {
    clearTimeout(timer);
    if (forceKill) clearTimeout(forceKill);
    if (failure) signal("SIGKILL");
    stdout?.close();
    stderr?.close();
    fs.closeSync(log);
  }
}

async function main() {
  if (process.platform === "win32") throw new Error("this native qualification requires Unix private storage");
  const [modelArg, outputArg, extra] = process.argv.slice(2);
  if (!modelArg || !outputArg || extra) {
    throw new Error("usage: node scripts/qualify-peer-native.mjs <model.gguf> <new-output-directory>");
  }
  const root = git(path.dirname(fileURLToPath(import.meta.url)), ["rev-parse", "--show-toplevel"]);
  if (git(root, ["status", "--porcelain", "--untracked-files=no"])) {
    throw new Error("commit or stash tracked edits before qualifying an exact source revision");
  }
  const sourceSha = git(root, ["rev-parse", "HEAD"]);
  const model = fs.realpathSync(modelArg);
  if (!fs.statSync(model).isFile()) throw new Error("model path must name an ordinary GGUF file");
  const output = path.resolve(outputArg);
  fs.mkdirSync(output, { mode: 0o700 });
  const temporary = fs.mkdtempSync(path.join(os.tmpdir(), "loom-peer-native-"));
  const worktree = path.join(temporary, "source");
  const engine = "crates/native/crates/llama-native-engine/src/lib.rs";
  const adapter = "products/loom/crates/loom-backend-llama/src/independent.rs";
  const target = path.join(root, "target", "peer-native-qualification");
  const tracePath = path.join(output, "decode.jsonl");
  const mappingPath = path.join(output, "mapping.jsonl");
  for (const filename of [tracePath, mappingPath]) {
    fs.writeFileSync(filename, "", { flag: "wx", mode: 0o600 });
  }
  let attached = false;
  try {
    git(root, ["worktree", "add", "--detach", worktree, sourceSha]);
    attached = true;
    const original = fs.readFileSync(path.join(worktree, engine), "utf8");
    const instrumented = instrumentDecodeSource(original);
    fs.writeFileSync(path.join(worktree, engine), instrumented);
    const originalAdapter = fs.readFileSync(path.join(worktree, adapter), "utf8");
    const instrumentedAdapter = instrumentIndependentSource(originalAdapter);
    fs.writeFileSync(path.join(worktree, adapter), instrumentedAdapter);
    const patch = `${git(worktree, ["diff", "--", engine, adapter])}\n`;
    fs.writeFileSync(path.join(output, "instrumentation.patch"), patch, { flag: "wx", mode: 0o600 });
    const env = {
      ...process.env,
      CARGO_TARGET_DIR: target,
      MOM_LLAMA_MODEL_PATH: model,
      LOOM_NATIVE_BATCH_TRACE_PATH: tracePath,
      LOOM_NATIVE_BATCH_MAPPING_PATH: mappingPath,
      LOOM_NATIVE_BATCH_OUTPUT_PATH: output,
      LOOM_QUALIFICATION_SOURCE_SHA: sourceSha,
    };
    const buildArgs = ["test", "--no-run", "--locked", "--release", "--package", "tauri-plugin-loom", "--lib", "--message-format=json"];
    const manifest = {
      source_sha: sourceSha,
      instrumented_build: true,
      native_source_sha256: sha256(original),
      instrumented_native_source_sha256: sha256(instrumented),
      adapter_source_sha256: sha256(originalAdapter),
      instrumented_adapter_source_sha256: sha256(instrumentedAdapter),
      instrumentation_patch_sha256: sha256(patch),
      command: ["cargo", ...buildArgs],
      test_name: nativeTestName,
      target_os: os.platform(),
      target_arch: os.arch(),
      os_release: os.release(),
      production_peer_executor: true,
      physical_networks: false,
      packaged_app: false,
      continuous_admission: false,
    };
    fs.writeFileSync(path.join(output, "build-intent.json"), JSON.stringify(manifest, null, 2), { flag: "wx", mode: 0o600 });
    const executables = new Set();
    await execute("cargo", buildArgs, {
      cwd: worktree, env, output, logName: "build.log",
      onStdout(line) { const executable = parseTestExecutable(line); if (executable) executables.add(executable); },
    });
    if (executables.size !== 1) throw new Error("Cargo did not identify exactly one plugin libtest executable");
    const [binary] = executables;
    if (!path.isAbsolute(binary) || !path.resolve(binary).startsWith(`${target}${path.sep}`)) {
      throw new Error("Cargo selected a test executable outside the qualification target directory");
    }
    const executableSha = await fileHash(binary);
    let traceBytes = 0;
    let mappingBytes = 0;
    await execute(binary, ["--exact", nativeTestName, "--ignored", "--nocapture", "--test-threads=1"], {
      cwd: worktree, env, output, logName: "native-run.log",
      onStderr(line) {
        const trace = parseTraceLine(line);
        if (trace) {
          const encoded = `${JSON.stringify(trace)}\n`;
          traceBytes += Buffer.byteLength(encoded);
          if (traceBytes > 8 * 1024 * 1024) throw new Error("native trace exceeded 8 MiB");
          fs.appendFileSync(tracePath, encoded);
        }
        const mapping = parseMappingLine(line);
        if (mapping) {
          const encoded = `${JSON.stringify(mapping)}\n`;
          mappingBytes += Buffer.byteLength(encoded);
          if (mappingBytes > 64 * 1024) throw new Error("native mapping exceeded 64 KiB");
          fs.appendFileSync(mappingPath, encoded);
        }
      },
    });
    const qualification = JSON.parse(fs.readFileSync(path.join(output, "qualification.json"), "utf8"));
    if (qualification.shared_decode_observed !== true
        || qualification.production_peer_executor !== true
        || qualification.native_model_released !== true
        || qualification.source_sha !== sourceSha) {
      throw new Error("the production peer test did not produce source-bound native qualification");
    }
    fs.writeFileSync(path.join(output, "receipt.json"), JSON.stringify({
      ...manifest,
      executable_sha256: executableSha,
      decode_trace_sha256: await fileHash(tracePath),
      mapping_trace_sha256: await fileHash(mappingPath),
      qualification_sha256: await fileHash(path.join(output, "qualification.json")),
      completed_at: new Date().toISOString(),
      result: "native_peer_executor_passed",
    }, null, 2), { flag: "wx", mode: 0o600 });
    console.log(`Native peer executor qualification passed: ${output}`);
  } finally {
    if (attached) git(root, ["worktree", "remove", "--force", worktree]);
    fs.rmSync(temporary, { recursive: true, force: true });
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch((error) => {
    console.error(error.message);
    process.exitCode = 1;
  });
}
