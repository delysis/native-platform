use anyhow::{Context, Result, ensure};
use std::{fs, path::Path, process::Command};

pub fn run(root: &Path, arguments: &[String]) -> Result<()> {
    ensure!(
        cfg!(target_os = "macos"),
        "macOS smoke support requires macOS"
    );
    ensure!(
        arguments.len() == 1,
        "usage: cargo xtask macos-smoke-support <output-directory>"
    );
    let output = Path::new(&arguments[0]);
    fs::create_dir_all(output).context("create smoke support output directory")?;
    let mut sources = fs::read_dir(root.join("scripts/macos-smoke-support"))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    sources.retain(|path| {
        path.extension()
            .is_some_and(|extension| extension == "swift")
    });
    sources.sort();
    ensure!(!sources.is_empty(), "no macOS smoke support sources found");
    for source in &sources {
        let name = source.file_stem().context("smoke support source name")?;
        let executable = output.join(name);
        let status = Command::new("xcrun")
            .arg("swiftc")
            .arg(source)
            .arg("-o")
            .arg(&executable)
            .status()
            .with_context(|| format!("compile {}", source.display()))?;
        ensure!(
            status.success(),
            "Swift compilation failed: {}",
            source.display()
        );
    }
    println!("compiled {} macOS smoke helpers", sources.len());
    Ok(())
}
