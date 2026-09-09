use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[derive(Deserialize)]
struct Registry {
    entries: Vec<Entry>,
}

#[derive(Deserialize)]
struct Entry {
    package: String,
    test_id: String,
    target: String,
    required_environment: Vec<String>,
}

pub fn run(root: &Path, args: &[String]) -> Result<()> {
    ensure!(
        args.len() >= 4 && args.len() % 2 == 0,
        "usage: cargo run -p xtask -- model-check MODEL SHA256 PACKAGE TEST_ID [PACKAGE TEST_ID ...]"
    );
    let model = PathBuf::from(&args[0])
        .canonicalize()
        .context("open model fixture")?;
    let mut file = File::open(&model)?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    let digest = format!("{:x}", hash.finalize());
    ensure!(
        digest == args[1],
        "model SHA-256 mismatch: expected {}, observed {digest}",
        args[1]
    );
    let registry: Registry =
        serde_json::from_reader(File::open(root.join("ci/ignored-tests.json"))?)?;
    let rustc = toolchain_program("rustc")?;
    let rustdoc = toolchain_program("rustdoc")?;
    let version = Command::new(&rustc).arg("--version").output()?;
    ensure!(version.status.success(), "could not read compiler identity");
    print!("{}", String::from_utf8_lossy(&version.stdout));
    println!("model: {}\nsha256: {digest}", model.display());

    for selection in args[2..].chunks_exact(2) {
        let (package, test_id) = (&selection[0], &selection[1]);
        let entry = registry
            .entries
            .iter()
            .find(|entry| &entry.package == package && &entry.test_id == test_id)
            .with_context(|| format!("unregistered test: {package} {test_id}"))?;
        ensure!(
            entry.target == "lib",
            "this CPU model lane supports library tests only"
        );
        ensure!(
            entry.required_environment.iter().all(|name| matches!(
                name.as_str(),
                "MOM_LLAMA_MODEL_PATH" | "MOM_LLAMA_MODEL_SHA256"
            )),
            "selected test requires a different fixture profile"
        );
        let invoke = |list: bool| -> Result<Output> {
            let mut command = Command::new("rustup");
            command
                .current_dir(root)
                .args([
                    "run",
                    "1.92.0",
                    "cargo",
                    "test",
                    "--locked",
                    "--package",
                    package,
                    "--lib",
                    test_id,
                    "--",
                    "--ignored",
                    "--exact",
                ])
                .env("RUSTC", &rustc)
                .env("RUSTDOC", &rustdoc)
                .env("MOM_LLAMA_MODEL_PATH", &model)
                .env("MOM_LLAMA_MODEL_SHA256", &digest);
            if list {
                command.arg("--list");
            } else {
                command.arg("--test-threads=1");
            }
            command.output().context("run exact model test")
        };
        let listed = invoke(true)?;
        ensure!(
            listed.status.success(),
            "test build/list failed: {}",
            String::from_utf8_lossy(&listed.stderr)
        );
        let expected = format!("{test_id}: test");
        ensure!(
            String::from_utf8_lossy(&listed.stdout)
                .lines()
                .filter(|line| *line == expected)
                .count()
                == 1,
            "selected test did not resolve exactly once: {package} {test_id}"
        );
        let executed = invoke(false)?;
        let stdout = String::from_utf8_lossy(&executed.stdout);
        print!("{stdout}");
        eprint!("{}", String::from_utf8_lossy(&executed.stderr));
        ensure!(executed.status.success() && stdout.lines().any(|line| line.starts_with("test result: ok. 1 passed; 0 failed; 0 ignored;")),
            "selected test failed or did not execute exactly once: {package} {test_id}");
    }
    Ok(())
}

fn toolchain_program(name: &str) -> Result<PathBuf> {
    let output = Command::new("rustup")
        .args(["which", "--toolchain", "1.92.0", name])
        .output()?;
    ensure!(output.status.success(), "Rust 1.92 {name} is unavailable");
    Ok(PathBuf::from(String::from_utf8(output.stdout)?.trim()))
}
