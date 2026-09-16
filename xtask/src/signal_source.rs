//! Assemble the exact worker sources alongside a release, without copying local state.

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, Output, Stdio};

const WORKER: &str = "products/loom/signal";
const RECEIPT: &str = "loom-signal-source.json";

#[derive(Debug, PartialEq, Eq, Serialize)]
struct Source {
    revision: String,
    tree: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct Package {
    name: String,
    version: String,
    source: Option<String>,
    license: Option<String>,
    license_file: Option<String>,
}

#[derive(Deserialize)]
struct Metadata {
    packages: Vec<Package>,
}

#[derive(Serialize)]
struct Receipt {
    schema: &'static str,
    source: Source,
    archive: String,
    archive_bytes: u64,
    archive_sha256: String,
    signal_lock_sha256: String,
    offline_resolution_verified: bool,
    packages: Vec<Package>,
}

pub(super) fn run(root: &Path, arguments: &[String]) -> Result<()> {
    ensure!(
        arguments.len() == 1,
        "usage: cargo xtask signal-source <output-directory>"
    );
    let receipt = assemble(root, Path::new(&arguments[0]))?;
    println!(
        "Signal source: {} ({})",
        receipt.archive, receipt.archive_sha256
    );
    Ok(())
}

fn assemble(root: &Path, output: &Path) -> Result<Receipt> {
    let source = clean_source(root)?;
    fs::create_dir_all(output).context("create source output directory")?;
    let output = fs::canonicalize(output)?;
    let name = format!("loom-signal-source-{}", source.revision);
    let archive_name = format!("{name}.tar.gz");
    ensure!(
        !output.join(&archive_name).exists() && !output.join(RECEIPT).exists(),
        "Signal source output already exists"
    );
    let staging = tempfile::tempdir_in(&output).context("stage Signal source archive")?;
    let source_tar = staging.path().join("source.tar");
    checked(
        Command::new("git")
            .current_dir(root)
            .args(["archive", "--format=tar", &source.revision])
            .stdout(File::create(&source_tar)?),
    )?;
    let extracted = staging.path().join(&name);
    fs::create_dir(&extracted)?;
    checked(
        Command::new("tar")
            .args(["-xf"])
            .arg(&source_tar)
            .arg("-C")
            .arg(&extracted),
    )?;
    let worker = extracted.join(WORKER);
    let signal_lock_sha256 = hash_file(&worker.join("Cargo.lock"))?;

    // Cargo owns source replacement and checksums; retain the existing path patches.
    let config = checked(cargo().current_dir(&worker).args([
        "vendor",
        "--locked",
        "--versioned-dirs",
        "vendor-cargo",
    ]))?;
    fs::create_dir(worker.join(".cargo"))
        .context("create worker vendor configuration (an existing configuration needs review)")?;
    fs::write(worker.join(".cargo/config.toml"), config.stdout)?;
    let empty_home = tempfile::tempdir_in(staging.path())?;
    let metadata = checked(
        cargo()
            .current_dir(&worker)
            .env("CARGO_HOME", empty_home.path())
            .args(["metadata", "--frozen", "--format-version", "1"]),
    )?;
    let mut metadata: Metadata = serde_json::from_slice(&metadata.stdout)?;
    metadata
        .packages
        .sort_by(|a, b| (&a.name, &a.version, &a.source).cmp(&(&b.name, &b.version, &b.source)));
    ensure!(
        signal_lock_sha256 == hash_file(&worker.join("Cargo.lock"))?,
        "Signal dependency lock changed during vendoring"
    );

    let archive = staging.path().join(&archive_name);
    checked(
        Command::new("tar")
            .args(["-czf"])
            .arg(&archive)
            .arg("-C")
            .arg(staging.path())
            .arg(&name),
    )?;
    let receipt = Receipt {
        schema: "loom.signal-source.v1",
        source,
        archive: archive_name,
        archive_bytes: fs::metadata(&archive)?.len(),
        archive_sha256: hash_file(&archive)?,
        signal_lock_sha256,
        offline_resolution_verified: true,
        packages: metadata.packages,
    };
    let receipt_path = staging.path().join(RECEIPT);
    let mut file = File::create(&receipt_path)?;
    serde_json::to_writer_pretty(&mut file, &receipt)?;
    writeln!(file)?;
    ensure!(
        clean_source(root)? == receipt.source,
        "source changed during archive preparation"
    );
    // Same-filesystem, no-clobber installation. The receipt is the completion marker.
    fs::hard_link(&archive, output.join(&receipt.archive))
        .context("install source archive without overwriting")?;
    fs::hard_link(&receipt_path, output.join(RECEIPT))
        .context("install source receipt without overwriting (archive retained)")?;
    Ok(receipt)
}

fn cargo() -> Command {
    Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
}

fn clean_source(root: &Path) -> Result<Source> {
    let status = checked(Command::new("git").current_dir(root).args([
        "status",
        "--porcelain",
        "--untracked-files=normal",
    ]))?;
    ensure!(
        status.stdout.is_empty(),
        "Signal source distribution requires a clean source tree"
    );
    Ok(Source {
        revision: git_id(root, "HEAD")?,
        tree: git_id(root, "HEAD^{tree}")?,
    })
}

fn git_id(root: &Path, revision: &str) -> Result<String> {
    let output = checked(
        Command::new("git")
            .current_dir(root)
            .args(["rev-parse", revision]),
    )?;
    let id = String::from_utf8(output.stdout)?.trim().to_owned();
    ensure!(
        !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "invalid Git object ID"
    );
    Ok(id)
}

fn hash_file(path: &Path) -> Result<String> {
    let mut digest = Sha256::new();
    io::copy(&mut File::open(path)?, &mut digest)?;
    Ok(format!("{:x}", digest.finalize()))
}

fn checked(command: &mut Command) -> Result<Output> {
    let output = command
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("run {}", command.get_program().to_string_lossy()))?;
    ensure!(
        output.status.success(),
        "{} failed: {}",
        command.get_program().to_string_lossy(),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output)
}

#[cfg(all(test, unix))]
#[path = "signal_source_tests.rs"]
mod tests;
