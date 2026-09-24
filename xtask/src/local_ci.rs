//! A sequential macOS component gate, not a hosted-check or product-acceptance service.
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use serde_json::json;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const SCOPE: &str = "macOS components only; NOT packaged/native UI, model/hardware, Linux, Windows, fuzz, dependency-audit or hosted-CI qualification";
const GUARD_IDENTITY: &str = "import { readPinnedToolIdentity } from './scripts/ci/validate-ignored-tests.mjs'; console.log(JSON.stringify(readPinnedToolIdentity({repoRoot: process.cwd()}), null, 2));";

pub fn run(root: &Path, args: &[String]) -> Result<()> {
    ensure!(
        args.len() == 1,
        "usage: cargo run --locked -p xtask -- local-ci <new-evidence-directory>"
    );
    let root = root.canonicalize()?;
    utf8(root.as_os_str())?;
    let evidence = new_evidence(&root, Path::new(&args[0]))?;
    utf8(evidence.as_os_str())?;
    write_json(
        &evidence.join("run.json"),
        &json!({
            "status": "incomplete", "scope": SCOPE, "started_unix_ms": now_ms(),
            "root": root, "pid": std::process::id(),
            "os": env::consts::OS, "architecture": env::consts::ARCH,
            "completion_rule": "Only a complete summary.json with status component-pass qualifies this gate. Missing/truncated records mean incomplete."
        }),
    )?;
    println!("local-ci evidence: {}", evidence.display());
    let mut log = Recorder {
        root,
        evidence,
        path: env::var_os("PATH").unwrap_or_default(),
        rust_channel: None,
        next: 0,
    };
    let mut before = None;
    let mut cache_lock = None;
    let mut gate_complete = false;
    let mut cache = None;
    let attempt = (|| -> Result<()> {
        for name in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE"] {
            ensure!(
                env::var_os(name).is_none(),
                "unset {name}; qualification must observe the actual checkout/index"
            );
        }
        before = Some(log.source("start")?);
        before.as_ref().context("initial source")?.require_clean()?;
        ensure!(
            cfg!(target_os = "macos"),
            "local-ci requires macOS; other platforms are not qualified by this runner"
        );
        ensure!(
            env::current_dir()?.canonicalize()? == log.root,
            "invoke local-ci from the repository root"
        );
        // Refuse a stale executable even when its embedded workspace path still exists.
        ensure!(
            fs::read(log.root.join("xtask/src/local_ci.rs"))?.as_slice()
                == include_bytes!("local_ci.rs")
                && fs::read(log.root.join("xtask/src/main.rs"))?.as_slice()
                    == include_bytes!("main.rs"),
            "runner source differs from this executable; rebuild with pinned Cargo"
        );
        let target = shared_target(&log.root, env::var_os("CARGO_TARGET_DIR"))?;
        utf8(target.as_os_str())?;
        ensure!(
            env::var_os("CARGO_BUILD_TARGET_DIR").is_none(),
            "unset CARGO_BUILD_TARGET_DIR; use the documented shared target"
        );
        ensure!(
            !log.evidence.starts_with(&target),
            "evidence must also be outside the build cache"
        );
        cache_lock = Some(CacheLock::acquire(&target, &log.evidence)?);
        cache = Some(target.clone());
        let tools = Tools::resolve(&mut log, target)?;
        let mut steps = component_steps(&log, &tools)?;
        let plan = steps
            .iter()
            .map(|(name, command)| Ok(json!({"step": name, "command": describe(command)?})))
            .collect::<Result<Vec<_>>>()?;
        write_json(&log.evidence.join("plan.json"), &plan)?;
        for (name, command) in &mut steps {
            log.execute(name, command)?;
        }
        gate_complete = true;
        Ok(())
    })();

    // Also run this after a command/prerequisite failure; never turn a failed run green.
    let after = log.source("end");
    let mut errors = Vec::new();
    if let Err(error) = attempt {
        errors.push(format!("{error:#}"));
    }
    let unchanged = match (&before, &after) {
        (Some(start), Ok(end)) => stable_source(start, end).is_ok(),
        _ => false,
    };
    if !unchanged {
        errors.push(
            "source was not clean and unchanged at both boundaries; no qualification".to_owned(),
        );
    }
    if let Err(error) = &after {
        errors.push(format!("final source check: {error:#}"));
    }
    let cache_unchanged = cache.as_ref().is_some_and(|expected| {
        log.root
            .join("target")
            .canonicalize()
            .is_ok_and(|actual| &actual == expected)
    });
    if cache.is_some() && !cache_unchanged {
        errors.push("shared target path changed during the gate".to_owned());
    }
    let passed = gate_complete && unchanged && cache_unchanged && errors.is_empty();
    let summary = json!({
        "status": if passed { "component-pass" } else { "failed" }, "scope": SCOPE,
        "finished_unix_ms": now_ms(), "component_gate_complete": gate_complete,
        "source_unchanged": unchanged, "source_start": before, "source_end": after.ok(),
        "cache": cache, "cache_unchanged": cache_unchanged,
        "command_records": log.next, "errors": errors
    });
    // Publication is last. An interrupted write leaves no terminal success record.
    write_json(&log.evidence.join("summary.pending.json"), &summary)?;
    fs::rename(
        log.evidence.join("summary.pending.json"),
        log.evidence.join("summary.json"),
    )?;
    drop(cache_lock);
    ensure!(
        passed,
        "local-ci failed; retain {} and inspect summary.json and command logs",
        log.evidence.display()
    );
    println!(
        "local-ci component-pass: {}\n{SCOPE}",
        log.evidence.display()
    );
    Ok(())
}

fn new_evidence(root: &Path, requested: &Path) -> Result<PathBuf> {
    let absolute = env::current_dir()?.join(requested);
    let parent = absolute
        .parent()
        .context("evidence parent")?
        .canonicalize()
        .context("evidence parent must already exist")?;
    ensure!(
        !parent.starts_with(root),
        "evidence must be outside the source checkout"
    );
    let path = parent.join(
        absolute
            .file_name()
            .context("new evidence directory name")?,
    );
    let builder = fs::DirBuilder::new();
    #[cfg(unix)]
    let builder = {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = builder;
        builder.mode(0o700);
        builder
    };
    builder
        .create(&path)
        .context("evidence directory must be new (never reuse or erase a receipt)")?;
    Ok(path)
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

fn now_ms() -> Option<u128> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|value| value.as_millis())
}

fn utf8(value: &OsStr) -> Result<&str> {
    value
        .to_str()
        .context("receipts require UTF-8 paths, arguments and explicit environment values")
}

// Only explicit child overrides are recorded, not an inherited environment/credential dump.
fn describe(command: &Command) -> Result<serde_json::Value> {
    let arguments = command.get_args().map(utf8).collect::<Result<Vec<_>>>()?;
    let environment = command
        .get_envs()
        .map(|(name, value)| Ok((utf8(name)?, value.map(utf8).transpose()?)))
        .collect::<Result<Vec<_>>>()?;
    Ok(json!({
        "program": utf8(command.get_program())?, "argv": arguments,
        "cwd": command.get_current_dir(), "stdin": "null",
        "environment_overrides": environment
    }))
}

struct Recorder {
    root: PathBuf,
    evidence: PathBuf,
    path: OsString,
    rust_channel: Option<String>,
    next: usize,
}

impl Recorder {
    fn command(&self, program: impl AsRef<OsStr>) -> Command {
        let mut command = Command::new(program);
        command
            .current_dir(&self.root)
            .stdin(Stdio::null())
            .env("PATH", &self.path)
            .env("CI", "true")
            .env("CARGO_TERM_COLOR", "never")
            .env("CARGO_NET_OFFLINE", "true")
            .env("npm_config_offline", "true")
            .env("RUSTUP_AUTO_INSTALL", "0");
        if let Some(channel) = &self.rust_channel {
            command.env("RUSTUP_TOOLCHAIN", channel);
        }
        // Cargo injects these into xtask. Node's existing libtest guard rejects them.
        // shared_target proves removing CARGO_TARGET_DIR does not change the cache.
        // Tools::resolve validates Cargo's macOS launcher loader paths before the gate;
        // do not pass that launcher-only search path into the guarded Node process.
        for name in [
            "CARGO",
            "RUSTC",
            "RUSTDOC",
            "CARGO_TARGET_DIR",
            "DYLD_FALLBACK_LIBRARY_PATH",
        ] {
            command.env_remove(name);
        }
        command
    }

    fn execute(&mut self, name: &str, command: &mut Command) -> Result<PathBuf> {
        self.next += 1;
        let directory = self.evidence.join(format!("{:03}-{name}", self.next));
        fs::create_dir(&directory)?;
        write_json(
            &directory.join("command.json"),
            &json!({
                "status": "incomplete", "started_unix_ms": now_ms(), "command": describe(command)?
            }),
        )?;
        let stdout = File::create(directory.join("stdout.log"))?;
        let stderr = File::create(directory.join("stderr.log"))?;
        println!("local-ci: {name} ({})", directory.display());
        let start = Instant::now();
        let status = command
            .stdout(stdout.try_clone()?)
            .stderr(stderr.try_clone()?)
            .status();
        let log_error = stdout.sync_all().and_then(|()| stderr.sync_all()).err();
        let success = status.as_ref().is_ok_and(|value| value.success()) && log_error.is_none();
        write_json(
            &directory.join("result.json"),
            &json!({
                "status": if success { "passed" } else { "failed" },
                "finished_unix_ms": now_ms(), "elapsed_ms": start.elapsed().as_millis(),
                "exit_code": status.as_ref().ok().and_then(|value| value.code()),
                "exit_status": status.as_ref().ok().map(ToString::to_string),
                "spawn_or_wait_error": status.as_ref().err().map(ToString::to_string),
                "log_error": log_error.map(|error| error.to_string())
            }),
        )?;
        ensure!(
            success,
            "command {name} failed; see {}",
            directory.display()
        );
        Ok(directory.join("stdout.log"))
    }

    fn text(&mut self, name: &str, command: &mut Command) -> Result<String> {
        let stdout = self.execute(name, command)?;
        Ok(fs::read_to_string(stdout)?.trim().to_owned())
    }

    fn git(&self, args: &[&str]) -> Command {
        let mut command = self.command("git");
        command
            .args([
                "--no-optional-locks",
                "-c",
                "core.fsmonitor=false",
                "-c",
                "core.untrackedCache=false",
            ])
            .args(args);
        command
    }

    fn source(&mut self, phase: &str) -> Result<Source> {
        let mut command = self.git(&["rev-parse", "--show-toplevel"]);
        let top = self.text(&format!("{phase}-root"), &mut command)?;
        ensure!(
            Path::new(&top).canonicalize()? == self.root,
            "not the checkout's top-level directory"
        );
        let mut command = self.git(&["rev-parse", "--verify", "HEAD"]);
        let sha = self.text(&format!("{phase}-sha"), &mut command)?;
        let mut command = self.git(&["rev-parse", "--verify", &format!("{sha}^{{tree}}")]);
        let tree = self.text(&format!("{phase}-tree"), &mut command)?;
        let mut command = self.git(&["ls-files", "-v", "-z"]);
        let flags = fs::read(self.execute(&format!("{phase}-index-flags"), &mut command)?)?;
        // Sparse/assume-unchanged entries can hide modified or missing source.
        let full_index = flags
            .split(|byte| *byte == 0)
            .filter(|entry| !entry.is_empty())
            .all(|entry| entry.first() == Some(&b'H'));
        let mut command = self.git(&[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignore-submodules=none",
        ]);
        let status = fs::read(self.execute(&format!("{phase}-status"), &mut command)?)?;
        let mut command = self.git(&["rev-parse", "--verify", "HEAD"]);
        ensure!(
            self.text(&format!("{phase}-head-recheck"), &mut command)? == sha,
            "HEAD changed while observing the {phase} source boundary"
        );
        let source = Source {
            sha,
            tree,
            clean: status.is_empty(),
            full_index,
        };
        write_json(&self.evidence.join(format!("source-{phase}.json")), &source)?;
        Ok(source)
    }
}

#[derive(Debug, Serialize, PartialEq, Eq)]
struct Source {
    sha: String,
    tree: String,
    clean: bool,
    full_index: bool,
}

impl Source {
    fn require_clean(&self) -> Result<()> {
        ensure!(
            self.clean && self.full_index,
            "qualification requires clean tracked/index/untracked source and no sparse or assume-unchanged entries"
        );
        Ok(())
    }
}

fn stable_source(before: &Source, after: &Source) -> Result<()> {
    before.require_clean()?;
    after.require_clean()?;
    ensure!(before == after, "source SHA/tree changed during the gate");
    Ok(())
}

fn shared_target(root: &Path, configured: Option<OsString>) -> Result<PathBuf> {
    let default = root
        .join("target")
        .canonicalize()
        .context("target must already exist; bootstrap with the pinned Cargo command")?;
    ensure!(
        default == root.join("target") || !default.starts_with(root),
        "target must not redirect builds into another source directory"
    );
    if let Some(path) = configured {
        ensure!(!path.is_empty(), "CARGO_TARGET_DIR must not be empty");
        ensure!(
            root.join(path).canonicalize()? == default,
            "CARGO_TARGET_DIR must resolve to the same directory as checkout/target; the unchanged ignored-test guard cannot use a different target override"
        );
    }
    Ok(default)
}

// cargo run adds this macOS search path even in an otherwise clean shell. Accept
// only paths Cargo normally contributes, then REMOVE them from child environments.
// Other loader/compiler overrides still reach the unchanged guard and are rejected.
fn validate_launcher_loader(
    value: &OsStr,
    target: &Path,
    sysroot: &Path,
    home: Option<&OsStr>,
) -> Result<()> {
    if value.is_empty() {
        return Ok(());
    }
    let home_lib = home.map(|path| Path::new(path).join("lib"));
    for path in env::split_paths(value) {
        ensure!(
            path.is_absolute()
                && !path
                    .components()
                    .any(|part| part == std::path::Component::ParentDir),
            "launcher loader path must be absolute and contain no parent traversal"
        );
        let standard = path == Path::new("/usr/local/lib")
            || path == Path::new("/usr/lib")
            || home_lib.as_ref() == Some(&path);
        let built = path
            .canonicalize()
            .is_ok_and(|path| path.starts_with(target) || path.starts_with(sysroot.join("lib")));
        ensure!(
            standard || built,
            "unexpected DYLD_FALLBACK_LIBRARY_PATH entry; unset custom loader overrides before launching local-ci"
        );
    }
    Ok(())
}

// A cooperative lock, not a lock on arbitrary Cargo invocations. A killed runner leaves
// this file for the integrator to inspect; never steal a lock or delete its evidence.
struct CacheLock(PathBuf);
impl CacheLock {
    fn acquire(target: &Path, evidence: &Path) -> Result<Self> {
        let path = target.join(".local-ci.lock");
        write_json(
            &path,
            &json!({"pid": std::process::id(), "evidence": evidence}),
        )
        .context(
            "shared target is locked or unavailable; inspect .local-ci.lock, do not overlap builds",
        )?;
        Ok(Self(path))
    }
}
impl Drop for CacheLock {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_file(&self.0) {
            eprintln!("retain/check local-ci lock {}: {error}", self.0.display());
        }
    }
}

struct Tools {
    cargo: PathBuf,
    rustc: PathBuf,
    rustdoc: PathBuf,
    rustfmt: PathBuf,
    channel: String,
    pnpm: String,
    target: PathBuf,
}

impl Tools {
    fn resolve(log: &mut Recorder, target: PathBuf) -> Result<Self> {
        let toolchain: toml::Value =
            toml::from_str(&fs::read_to_string(log.root.join("rust-toolchain.toml"))?)?;
        let channel = toolchain
            .get("toolchain")
            .and_then(|value| value.get("channel"))
            .and_then(toml::Value::as_str)
            .context("exact Rust channel")?
            .to_owned();
        let package: serde_json::Value =
            serde_json::from_slice(&fs::read(log.root.join("package.json"))?)?;
        let pnpm = package["packageManager"]
            .as_str()
            .and_then(|value| value.strip_prefix("pnpm@"))
            .context("package.json must pin pnpm")?
            .to_owned();
        for version in [&channel, &pnpm] {
            ensure!(
                version.split('.').count() == 3
                    && version.split('.').all(|part| part.parse::<u64>().is_ok()),
                "tool pins must be exact stable versions"
            );
        }
        log.rust_channel = Some(channel.clone());
        let names = [
            "cargo",
            "rustc",
            "rustdoc",
            "rustfmt",
            "cargo-fmt",
            "cargo-clippy",
            "clippy-driver",
        ];
        let mut paths = Vec::new();
        for tool in names {
            let mut command = log.command("rustup");
            command.args(["which", "--toolchain", &channel, tool]);
            let path = PathBuf::from(log.text(&format!("resolve-{tool}"), &mut command)?);
            ensure!(
                path.is_absolute() && path.is_file(),
                "rustup did not return an installed {tool}"
            );
            paths.push(path.canonicalize()?);
        }
        let bin = paths[0].parent().context("toolchain bin directory")?;
        ensure!(
            paths.iter().all(|path| path.parent() == Some(bin)),
            "Rust tools must come from one installed toolchain"
        );
        if let Some(loader) = env::var_os("DYLD_FALLBACK_LIBRARY_PATH") {
            let home = env::var_os("HOME");
            write_json(
                &log.evidence.join("launcher-loader.json"),
                &json!({
                    "variable": "DYLD_FALLBACK_LIBRARY_PATH", "value": utf8(&loader)?,
                    "action": "removed for child commands; all entries must validate before the gate"
                }),
            )?;
            validate_launcher_loader(
                &loader,
                &target,
                bin.parent().context("toolchain root")?,
                home.as_deref(),
            )?;
        }
        log.path =
            env::join_paths(std::iter::once(bin.to_owned()).chain(env::split_paths(&log.path)))?;
        let tools = Self {
            cargo: paths[0].clone(),
            rustc: paths[1].clone(),
            rustdoc: paths[2].clone(),
            rustfmt: paths[3].clone(),
            channel,
            pnpm,
            target,
        };
        for (name, path) in [
            ("CARGO", &tools.cargo),
            ("RUSTC", &tools.rustc),
            ("RUSTDOC", &tools.rustdoc),
            ("RUSTFMT", &tools.rustfmt),
        ] {
            if let Some(inherited) = env::var_os(name) {
                ensure!(
                    Path::new(&inherited).canonicalize()? == *path,
                    "{name} is not the pinned rustup binary; use CONTRIBUTING.md's bootstrap command"
                );
            }
        }
        for (tool, path) in names.iter().zip(&paths) {
            let mut command = log.command(path);
            command.arg("--version");
            if matches!(*tool, "cargo" | "rustc" | "rustdoc") {
                command.arg("--verbose");
            }
            let version = log.text(&format!("version-{tool}"), &mut command)?;
            if matches!(*tool, "cargo" | "rustc" | "rustdoc") {
                ensure!(
                    version.split_whitespace().nth(1) == Some(tools.channel.as_str()),
                    "{tool} version differs from rust-toolchain.toml"
                );
            }
        }
        for (name, program, args) in [
            ("git", "git", vec!["--version"]),
            ("rustup", "rustup", vec!["--version"]),
            ("npm", "npm", vec!["--version"]),
            ("npx", "npx", vec!["--version"]),
            ("macos", "sw_vers", vec![]),
            ("host", "uname", vec!["-m"]),
            ("swift", "xcrun", vec!["swiftc", "--version"]),
            ("clang", "xcrun", vec!["clang", "--version"]),
            ("sdk", "xcrun", vec!["--show-sdk-path"]),
            ("cmake", "cmake", vec!["--version"]),
        ] {
            let mut command = log.command(program);
            log.execute(&format!("version-{name}"), command.args(args))?;
        }
        let mut node = log.command("node");
        ensure!(
            log.text("version-node", node.arg("--version"))?
                .starts_with("v22."),
            "Node 22 is required, matching full CI"
        );
        let mut pnpm = tools.pnpm_command(log, &["--version"]);
        ensure!(
            log.text("version-pnpm", &mut pnpm)? == tools.pnpm,
            "cached pnpm does not match package.json"
        );
        // Reuse, rather than reimplement or relax, the existing environment/config guard.
        let mut guard = log.command("node");
        log.execute(
            "guard-prerequisites",
            guard.args(["--input-type=module", "-e", GUARD_IDENTITY]),
        )?;
        write_json(
            &log.evidence.join("tools.json"),
            &json!({
                "rust_channel": tools.channel, "cargo": tools.cargo, "rustc": tools.rustc,
                "rustdoc": tools.rustdoc, "rustfmt": tools.rustfmt, "pnpm": tools.pnpm, "target": tools.target,
                "resolution": "rustup which; actual toolchain bin precedes inherited PATH; npx offline with installation refused"
            }),
        )?;
        Ok(tools)
    }

    fn cargo_command(&self, log: &Recorder, args: &[&str]) -> Command {
        let mut command = log.command(&self.cargo);
        command
            .args(args)
            .env("CARGO", &self.cargo)
            .env("RUSTC", &self.rustc)
            .env("RUSTDOC", &self.rustdoc)
            .env("RUSTFMT", &self.rustfmt)
            .env("RUSTUP_TOOLCHAIN", &self.channel)
            // Use the guard's spelling too, avoiding alias-path fingerprint churn.
            .env("CARGO_TARGET_DIR", log.root.join("target"));
        command
    }

    fn pnpm_command(&self, log: &Recorder, args: &[&str]) -> Command {
        let mut command = log.command("npx");
        command.args(["--offline", "--yes=false", &format!("pnpm@{}", self.pnpm)]);
        if args.first() == Some(&"--filter") {
            command.arg("--fail-if-no-match");
        }
        command.args(args);
        command
    }
}

fn component_steps(log: &Recorder, tools: &Tools) -> Result<Vec<(&'static str, Command)>> {
    let cargo = |args: &[&str]| tools.cargo_command(log, args);
    let node = |args: &[&str]| {
        let mut command = log.command("node");
        command.args(args);
        command
    };
    let pnpm = |args: &[&str]| tools.pnpm_command(log, args);
    // Discover CI Node tests, not a second list of selected tests/packages. Serialize test
    // files too: some ask Cargo for metadata. Existing CI scripts own their selectors.
    let mut tests = fs::read_dir(log.root.join("scripts/ci"))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    tests.retain(|path| {
        path.file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| name.starts_with("test-") && name.ends_with(".mjs"))
    });
    tests.sort();
    ensure!(!tests.is_empty(), "no CI Node tests found");
    let mut node_tests = node(&["--test", "--test-concurrency=1"]);
    node_tests.args(tests);
    let mut helpers = cargo(&[
        "run",
        "--locked",
        "-p",
        "xtask",
        "--",
        "macos-smoke-support",
    ]);
    helpers.arg(log.evidence.join("macos-smoke-support"));
    // Full-workspace commands subsume package/group selection. Frontend-only commands
    // mirror ci-full.yml; do not substitute product scripts that recursively build Rust.
    Ok(vec![
        ("fmt", cargo(&["fmt", "--all", "--", "--check"])),
        (
            "policy",
            cargo(&["run", "--locked", "-p", "xtask", "--", "policy"]),
        ),
        ("ci-node-tests", node_tests),
        (
            "current-docs",
            node(&["scripts/ci/validate-current-docs.mjs"]),
        ),
        (
            "sqlite-graph",
            cargo(&["tree", "--locked", "-i", "libsqlite3-sys"]),
        ),
        (
            "workspace-tests",
            cargo(&[
                "test",
                "--locked",
                "--workspace",
                "--all-targets",
                "--no-fail-fast",
            ]),
        ),
        (
            "doctests",
            cargo(&["test", "--locked", "--workspace", "--doc"]),
        ),
        (
            "clippy",
            cargo(&[
                "clippy",
                "--locked",
                "--workspace",
                "--all-targets",
                "--",
                "-D",
                "warnings",
            ]),
        ),
        (
            "ignored-test-inventory",
            node(&["scripts/ci/validate-ignored-tests.mjs", "--cargo-list"]),
        ),
        (
            "frontend-dependencies",
            pnpm(&["install", "--offline", "--frozen-lockfile"]),
        ),
        (
            "fte-check",
            pnpm(&["--filter", "free-token-energy", "run", "check:frontend"]),
        ),
        (
            "fte-test",
            pnpm(&["--filter", "free-token-energy", "run", "test:frontend"]),
        ),
        (
            "mom-check",
            pnpm(&["--filter", "@delysis/mom-llama", "run", "check:frontend"]),
        ),
        (
            "mom-test",
            pnpm(&["--filter", "@delysis/mom-llama", "run", "test:frontend"]),
        ),
        (
            "loom-test",
            pnpm(&["--filter", "@delysis/loom", "run", "test"]),
        ),
        (
            "loom-check",
            pnpm(&["--filter", "@delysis/loom", "run", "check"]),
        ),
        (
            "loom-build",
            pnpm(&["--filter", "@delysis/loom", "run", "build"]),
        ),
        (
            "webkit",
            pnpm(&["--filter", "@delysis/loom", "run", "test:browser"]),
        ),
        ("macos-smoke-helpers", helpers),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            loop {
                let path = env::temp_dir().join(format!(
                    "local-ci-test-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
                match fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("create isolated test directory: {error}"),
                }
            }
        }
        fn recorder(&self) -> Recorder {
            let root = self.0.join("source");
            fs::create_dir(&root).expect("source directory");
            let root = root.canonicalize().expect("canonical source");
            let evidence = new_evidence(&root, &self.0.join("evidence")).expect("new evidence");
            Recorder {
                root,
                evidence,
                path: env::var_os("PATH").unwrap_or_default(),
                rust_channel: None,
                next: 0,
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            // Only this test's exclusively created temporary directory; never user receipts.
            fs::remove_dir_all(&self.0).expect("remove isolated test fixture");
        }
    }
    fn read_json(path: &Path) -> serde_json::Value {
        serde_json::from_slice(&fs::read(path).expect("read receipt")).expect("valid JSON")
    }
    fn fixture_git(log: &mut Recorder, args: &[&str]) {
        let mut command = log.command("git");
        // Even focused tests must never redirect a Git write into the caller's checkout.
        for key in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_COMMON_DIR",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        ] {
            command.env_remove(key);
        }
        command
            .args([
                "-c",
                "init.templateDir=",
                "-c",
                "user.name=Local CI test",
                "-c",
                "user.email=local-ci@example.invalid",
                "-c",
                "commit.gpgsign=false",
            ])
            .arg("-c")
            .arg(format!(
                "core.hooksPath={}",
                log.root.join("no-hooks").display()
            ))
            .args(args);
        log.execute("fixture-git", &mut command)
            .expect("fixture Git command");
    }
    fn commit_fixture(log: &mut Recorder) {
        fixture_git(log, &["init", "--quiet"]);
        fs::write(log.root.join("source.txt"), "original\n").expect("fixture source");
        fixture_git(log, &["add", "source.txt"]);
        fixture_git(log, &["commit", "--quiet", "-m", "initial fixture"]);
    }

    #[test]
    fn existing_evidence_is_never_reused_and_source_cannot_hold_evidence() {
        let fixture = Fixture::new();
        let log = fixture.recorder();
        let sentinel = log.evidence.join("failed-receipt.txt");
        fs::write(&sentinel, "preserve me").expect("old receipt");
        assert!(new_evidence(&log.root, &log.evidence).is_err());
        assert_eq!(
            fs::read_to_string(sentinel).expect("preserved receipt"),
            "preserve me"
        );
        assert!(new_evidence(&log.root, &log.root.join("new-evidence")).is_err());
        assert!(!log.root.join("new-evidence").exists());
    }

    #[test]
    fn command_failure_preserves_both_streams_and_exit_status() {
        let fixture = Fixture::new();
        let mut log = fixture.recorder();
        #[cfg(unix)]
        let mut command = {
            let mut command = log.command("sh");
            command.args([
                "-c",
                "printf 'stdout-sentinel\\n'; printf 'stderr-sentinel\\n' >&2; exit 17",
            ]);
            command
        };
        #[cfg(windows)]
        let mut command = {
            let mut command = log.command("cmd");
            command.args([
                "/D",
                "/C",
                "echo stdout-sentinel & echo stderr-sentinel 1>&2 & exit /b 17",
            ]);
            command
        };
        assert!(log.execute("expected-failure", &mut command).is_err());
        let directory = log.evidence.join("001-expected-failure");
        let result = read_json(&directory.join("result.json"));
        assert_eq!(result["status"], "failed");
        assert_eq!(result["exit_code"], 17);
        assert!(write_json(&directory.join("result.json"), &json!({"status": "passed"})).is_err());
        assert_eq!(
            read_json(&directory.join("result.json"))["status"],
            "failed"
        );
        assert!(
            fs::read_to_string(directory.join("stdout.log"))
                .expect("stdout")
                .contains("stdout-sentinel")
        );
        assert!(
            fs::read_to_string(directory.join("stderr.log"))
                .expect("stderr")
                .contains("stderr-sentinel")
        );
        assert_eq!(
            read_json(&directory.join("command.json"))["status"],
            "incomplete"
        );
        assert!(!log.evidence.join("summary.json").exists());
    }

    #[test]
    fn spawn_failure_is_a_recorded_failure_not_a_missing_step() {
        let fixture = Fixture::new();
        let mut log = fixture.recorder();
        let mut command = log.command(log.root.join("absent-program"));
        assert!(log.execute("spawn", &mut command).is_err());
        let result = read_json(&log.evidence.join("001-spawn/result.json"));
        assert_eq!(result["status"], "failed");
        assert!(result["exit_code"].is_null());
        assert!(
            result["spawn_or_wait_error"]
                .as_str()
                .is_some_and(|value| !value.is_empty())
        );
    }

    #[cfg(unix)]
    #[test]
    fn child_signal_cannot_be_reported_as_success() {
        let fixture = Fixture::new();
        let mut log = fixture.recorder();
        let mut command = log.command("sh");
        command.args(["-c", "printf 'partial-output'; kill -TERM $$"]);
        assert!(log.execute("signal", &mut command).is_err());
        let result = read_json(&log.evidence.join("001-signal/result.json"));
        assert_eq!(result["status"], "failed");
        assert!(result["exit_code"].is_null());
        assert!(
            result["exit_status"]
                .as_str()
                .expect("status")
                .contains("signal")
        );
        assert_eq!(
            fs::read_to_string(log.evidence.join("001-signal/stdout.log")).expect("partial log"),
            "partial-output"
        );
    }

    #[test]
    fn dirty_staged_untracked_and_hidden_changes_refuse_qualification() {
        let fixture = Fixture::new();
        let mut log = fixture.recorder();
        commit_fixture(&mut log);
        let clean = log.source("clean").expect("clean source");
        clean.require_clean().expect("qualifiable starting source");
        fs::write(log.root.join("source.txt"), "changed\n").expect("dirty source");
        assert!(stable_source(&clean, &log.source("dirty").expect("dirty snapshot")).is_err());
        fixture_git(&mut log, &["add", "source.txt"]);
        assert!(
            log.source("staged")
                .expect("staged snapshot")
                .require_clean()
                .is_err()
        );
        fs::write(log.root.join("source.txt"), "original\n").expect("restore test source");
        fixture_git(&mut log, &["add", "source.txt"]);
        fs::write(log.root.join("untracked.txt"), "not committed").expect("untracked file");
        assert!(
            log.source("untracked")
                .expect("untracked snapshot")
                .require_clean()
                .is_err()
        );
        fs::remove_file(log.root.join("untracked.txt")).expect("remove fixture file");
        fixture_git(
            &mut log,
            &["update-index", "--assume-unchanged", "source.txt"],
        );
        assert!(
            log.source("hidden")
                .expect("hidden snapshot")
                .require_clean()
                .is_err()
        );
    }

    #[test]
    fn same_tree_commit_drift_still_invalidates_source() {
        let fixture = Fixture::new();
        let mut log = fixture.recorder();
        commit_fixture(&mut log);
        let before = log.source("before").expect("initial snapshot");
        stable_source(&before, &log.source("unchanged").expect("same source"))
            .expect("unchanged source");
        fixture_git(
            &mut log,
            &[
                "commit",
                "--quiet",
                "--allow-empty",
                "-m",
                "different commit same tree",
            ],
        );
        let after = log.source("after").expect("final snapshot");
        assert_eq!(before.tree, after.tree);
        assert_ne!(before.sha, after.sha);
        assert!(stable_source(&before, &after).is_err());
    }

    #[test]
    fn shared_cache_requires_the_guards_default_path_and_exclusive_ownership() {
        let fixture = Fixture::new();
        let log = fixture.recorder();
        let target = log.root.join("target");
        let other = fixture.0.join("other-target");
        fs::create_dir(&target).expect("target");
        fs::create_dir(&other).expect("other target");
        assert_eq!(
            shared_target(&log.root, Some(target.clone().into_os_string())).expect("shared target"),
            target.canonicalize().expect("canonical target")
        );
        assert!(shared_target(&log.root, Some(other.clone().into_os_string())).is_err());
        let lock = CacheLock::acquire(&target, &log.evidence).expect("first lock");
        assert!(CacheLock::acquire(&target, &log.evidence).is_err());
        drop(lock);
        drop(CacheLock::acquire(&target, &log.evidence).expect("released lock"));
        #[cfg(unix)]
        {
            fs::remove_dir(&target).expect("empty target");
            std::os::unix::fs::symlink(&other, &target).expect("existing integrator-managed alias");
            assert_eq!(
                shared_target(&log.root, Some(other.clone().into_os_string()))
                    .expect("same physical target"),
                other.canonicalize().expect("canonical other target")
            );
        }
    }

    #[test]
    fn launcher_loader_cleanup_rejects_unrelated_and_relative_search_paths() {
        let fixture = Fixture::new();
        let log = fixture.recorder();
        let target = log.root.join("target");
        let sysroot = log.root.join("toolchain");
        let deps = target.join("debug/deps");
        let rustlib = sysroot.join("lib/rustlib/host/lib");
        fs::create_dir_all(&deps).expect("build libraries");
        fs::create_dir_all(&rustlib).expect("toolchain libraries");
        let paths = env::join_paths([deps, rustlib]).expect("Cargo launcher path");
        validate_launcher_loader(&paths, &target, &sysroot, None)
            .expect("known launcher paths can be removed");
        assert!(validate_launcher_loader(OsStr::new("relative"), &target, &sysroot, None).is_err());
        assert!(validate_launcher_loader(log.root.as_os_str(), &target, &sysroot, None).is_err());
        let escape = target.join("../unrelated");
        assert!(validate_launcher_loader(escape.as_os_str(), &target, &sysroot, None).is_err());
    }

    #[test]
    fn fixed_plan_preserves_strict_gates_and_guarded_environments() {
        let fixture = Fixture::new();
        let log = fixture.recorder();
        let directory = log.root.join("scripts/ci");
        fs::create_dir_all(&directory).expect("CI directory");
        fs::write(directory.join("test-b.mjs"), "").expect("test file");
        fs::write(directory.join("test-a.mjs"), "").expect("test file");
        fs::write(directory.join("not-a-test.mjs"), "").expect("non-test file");
        let tools = Tools {
            cargo: "pinned/cargo".into(),
            rustc: "pinned/rustc".into(),
            rustdoc: "pinned/rustdoc".into(),
            rustfmt: "pinned/rustfmt".into(),
            channel: "1.92.0".to_owned(),
            pnpm: "11.16.0".to_owned(),
            target: log.root.join("target"),
        };
        let steps = component_steps(&log, &tools).expect("fixed plan");
        let args = |name: &str| {
            let command = &steps
                .iter()
                .find(|(step, _)| *step == name)
                .expect("planned step")
                .1;
            command
                .get_args()
                .map(|arg| utf8(arg).expect("UTF-8 argument").to_owned())
                .collect::<Vec<_>>()
        };
        assert!(args("workspace-tests").contains(&"--no-fail-fast".to_owned()));
        assert!(args("workspace-tests").contains(&"--all-targets".to_owned()));
        assert!(args("doctests").contains(&"--doc".to_owned()));
        assert!(args("clippy").ends_with(&[
            "--".to_owned(),
            "-D".to_owned(),
            "warnings".to_owned()
        ]));
        assert_eq!(
            args("ignored-test-inventory"),
            ["scripts/ci/validate-ignored-tests.mjs", "--cargo-list"]
        );
        assert!(args("macos-smoke-helpers").contains(&"macos-smoke-support".to_owned()));
        assert!(args("webkit").contains(&"--fail-if-no-match".to_owned()));
        assert!(args("webkit").contains(&"--yes=false".to_owned()));
        let node = args("ci-node-tests");
        assert_eq!(&node[..2], ["--test", "--test-concurrency=1"]);
        assert_eq!(node.len(), 4);
        assert!(node[2].ends_with("test-a.mjs") && node[3].ends_with("test-b.mjs"));
        let guard = &steps
            .iter()
            .find(|(name, _)| *name == "ignored-test-inventory")
            .expect("guard")
            .1;
        for key in [
            "CARGO",
            "RUSTC",
            "RUSTDOC",
            "CARGO_TARGET_DIR",
            "DYLD_FALLBACK_LIBRARY_PATH",
        ] {
            assert!(
                guard
                    .get_envs()
                    .any(|(name, value)| name == OsStr::new(key) && value.is_none())
            );
        }
        let command = describe(guard).expect("command description");
        assert!(command["argv"].is_array());
        assert_eq!(command["program"], "node");
    }
}
