#!/usr/bin/env node
// Opt-in diagnostic only. Instrument an isolated exact-revision worktree, never
// the user's checkout or release sources. Successful component qualification is
// not packaged-app acceptance, continuous admission, or a physical-network test.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import crypto from "node:crypto";
import { spawn, spawnSync } from "node:child_process";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";

export const decodeSite = `        context
            .decode(&mut batch)
            .map_err(|error| native_decode_error("failed to decode generation batch", error))?;`;

export function instrumentDecodeSource(source) {
  if (source.includes("LOOM_NATIVE_BATCH_TRACE") || source.split(decodeSite).length !== 2) {
    throw new Error("native decode instrumentation no longer matches exactly one reviewed site");
  }
  // Emission follows successful context.decode, not scheduler admission or a
  // fixture callback. Record actual rows and native sequence IDs, never prompts.
  return source.replace(decodeSite, `${decodeSite}
        eprintln!("LOOM_NATIVE_BATCH_TRACE {}", serde_json::json!({
            "kind": "loom_native_decode_batch_v1",
            "request_id": request.request_id,
            "case_ids": next_tokens.iter().map(|(index, _)| branches[*index].request.branch_id.as_str()).collect::<Vec<_>>(),
            "sequence_ids": next_tokens.iter().map(|(index, _)| branches[*index].sequence_id).collect::<Vec<_>>(),
            "generated_counts": next_tokens.iter().map(|(index, _)| branches[*index].generated).collect::<Vec<_>>(),
        }));`);
}

export function parseTraceLine(line) {
  const prefix = "LOOM_NATIVE_BATCH_TRACE ";
  if (!line.startsWith(prefix)) return null;
  const record = JSON.parse(line.slice(prefix.length));
  if (record.kind !== "loom_native_decode_batch_v1") throw new Error("invalid native trace kind");
  return record;
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

async function execute(command, args, { cwd, env, output, logName, tracePath = null }) {
  const log = fs.openSync(path.join(output, logName), "wx", 0o600);
  let logBytes = 0;
  let traceBytes = 0;
  let failure = null;
  const child = spawn(command, args, { cwd, env, stdio: ["ignore", "pipe", "pipe"] });
  let forceKill;
  const terminate = (error) => {
    failure ??= error;
    child.kill("SIGTERM");
    forceKill ??= setTimeout(() => child.kill("SIGKILL"), 5000);
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
  const lines = tracePath ? createInterface({ input: child.stderr }) : null;
  lines?.on("line", (line) => {
    try {
      const record = parseTraceLine(line);
      if (!record) return;
      const encoded = `${JSON.stringify(record)}\n`;
      traceBytes += Buffer.byteLength(encoded);
      if (traceBytes > 8 * 1024 * 1024) throw new Error("native trace exceeded 8 MiB");
      fs.appendFileSync(tracePath, encoded);
    } catch (error) {
      terminate(error);
    }
  });
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
    lines?.close();
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
  const target = path.join(root, "target", "peer-native-qualification");
  const tracePath = path.join(output, "decode.jsonl");
  fs.writeFileSync(tracePath, "", { flag: "wx", mode: 0o600 });
  let attached = false;
  try {
    git(root, ["worktree", "add", "--detach", worktree, sourceSha]);
    attached = true;
    const original = fs.readFileSync(path.join(worktree, engine), "utf8");
    const instrumented = instrumentDecodeSource(original);
    fs.writeFileSync(path.join(worktree, engine), instrumented);
    const patch = git(worktree, ["diff", "--", engine]);
    fs.writeFileSync(path.join(output, "instrumentation.patch"), `${patch}\n`, { flag: "wx", mode: 0o600 });
    const env = {
      ...process.env,
      CARGO_TARGET_DIR: target,
      LOOM_NATIVE_BATCH_TRACE_PATH: tracePath,
      LOOM_QUALIFICATION_SOURCE_SHA: sourceSha,
    };
    const buildArgs = ["build", "--locked", "--release", "--package", "tauri-plugin-loom", "--example", "qualify_peer_native"];
    const manifest = {
      source_sha: sourceSha,
      instrumented_build: true,
      native_source_sha256: sha256(original),
      instrumented_native_source_sha256: sha256(instrumented),
      instrumentation_patch_sha256: sha256(`${patch}\n`),
      command: ["cargo", ...buildArgs],
      physical_networks: false,
      packaged_tauri_adapter: false,
      continuous_admission: false,
    };
    fs.writeFileSync(path.join(output, "build-intent.json"), JSON.stringify(manifest, null, 2), { flag: "wx", mode: 0o600 });
    await execute("cargo", buildArgs, { cwd: worktree, env, output, logName: "build.log" });
    const binary = path.join(target, "release", "examples", "qualify_peer_native");
    const executableSha = await fileHash(binary);
    await execute(binary, [model, output], { cwd: worktree, env, output, logName: "native-run.log", tracePath });
    const qualification = JSON.parse(fs.readFileSync(path.join(output, "qualification.json"), "utf8"));
    if (qualification.shared_decode_observed !== true || qualification.source_sha !== sourceSha) {
      throw new Error("native driver did not produce source-bound shared-decode qualification");
    }
    fs.writeFileSync(path.join(output, "receipt.json"), JSON.stringify({
      ...manifest,
      executable_sha256: executableSha,
      decode_trace_sha256: await fileHash(tracePath),
      qualification_sha256: await fileHash(path.join(output, "qualification.json")),
      completed_at: new Date().toISOString(),
      result: "native_component_passed",
    }, null, 2), { flag: "wx", mode: 0o600 });
    console.log(`Native component qualification passed: ${output}`);
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
