//! Repository policy, scoped to Git-owned files rather than ignored tool caches.
//! Parse dependency manifests semantically; prose mentioning Python is allowed.
use anyhow::{Context, Result, ensure};
use std::fs::{self, File};
use std::io::Read as _;
use std::path::Path;
use std::process::Command;

const HEADER_BYTES: u64 = 4096;
const MANIFEST_BYTES: u64 = 16 * 1024 * 1024;

pub fn check(root: &Path) -> Result<()> {
    let output = Command::new("git")
        .args(["ls-files", "--cached", "-z", "--full-name"])
        .current_dir(root)
        .output()
        .context("list Git-owned files for the no-Python policy")?;
    ensure!(
        output.status.success(),
        "cannot list Git-owned files: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let files = std::str::from_utf8(&output.stdout).context("Git-owned paths must be UTF-8")?;
    for relative in files.split('\0').filter(|path| !path.is_empty()) {
        let path = Path::new(relative);
        ensure!(
            !forbidden_path(path),
            "Python file is forbidden: {relative}"
        );
        let absolute = root.join(path);
        let metadata =
            fs::symlink_metadata(&absolute).with_context(|| format!("inspect {relative}"))?;
        if metadata.file_type().is_symlink() {
            let target = fs::read_link(&absolute)?;
            ensure!(
                !forbidden_path(&target),
                "Python symlink target is forbidden: {relative}"
            );
            ensure!(
                !target
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(python_interpreter),
                "Python interpreter symlink is forbidden: {relative}"
            );
            continue;
        }
        // Git submodules are separate repositories, not recursively owned files.
        if !metadata.is_file() {
            continue;
        }
        let mut header = Vec::new();
        File::open(&absolute)?
            .take(HEADER_BYTES)
            .read_to_end(&mut header)?;
        ensure!(
            !python_shebang(&header),
            "Python shebang is forbidden: {relative}"
        );
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if matches!(name.as_str(), "cargo.toml" | "cargo.lock" | "package.json") {
            ensure!(
                metadata.len() <= MANIFEST_BYTES,
                "oversized dependency manifest: {relative}"
            );
            let text = fs::read_to_string(&absolute).with_context(|| format!("read {relative}"))?;
            check_manifest(&name, &text)
                .with_context(|| format!("Python dependency policy: {relative}"))?;
        }
    }
    Ok(())
}

fn forbidden_path(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    matches!(
        extension.as_str(),
        "py" | "pyw" | "pyi" | "pyc" | "pyo" | "pyz" | "pyd" | "ipynb" | "whl" | "egg"
    ) || matches!(
        name.as_str(),
        "pyproject.toml"
            | "pipfile"
            | "pipfile.lock"
            | "poetry.lock"
            | "uv.lock"
            | "pdm.lock"
            | "pylock.toml"
            | ".python-version"
            | "tox.ini"
            | "pytest.ini"
    ) || ((name == "requirements.txt"
        || name == "requirements.in"
        || name.starts_with("requirements-")
        || name.starts_with("requirements."))
        && matches!(extension.as_str(), "txt" | "in"))
        || (path.components().any(|part| {
            part.as_os_str()
                .to_str()
                .is_some_and(|part| part.eq_ignore_ascii_case("requirements"))
        }) && matches!(extension.as_str(), "txt" | "in"))
        || path
            .components()
            .any(|part| part.as_os_str() == "__pycache__")
}

fn python_interpreter(value: &str) -> bool {
    let name = value
        .trim_matches(['\'', '"'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let name = name.strip_suffix(".exe").unwrap_or(&name);
    if name == "py" {
        return true;
    }
    ["python", "pypy"].iter().any(|prefix| {
        name.strip_prefix(prefix).is_some_and(|suffix| {
            suffix.is_empty()
                || suffix == "w"
                || (suffix.starts_with(|ch: char| ch.is_ascii_digit())
                    && suffix.bytes().all(|ch| {
                        ch.is_ascii_digit() || matches!(ch, b'.' | b'm' | b'd' | b't' | b'w')
                    }))
        })
    })
}

fn python_shebang(bytes: &[u8]) -> bool {
    let line = String::from_utf8_lossy(bytes);
    let Some(command) = line.lines().next().and_then(|line| line.strip_prefix("#!")) else {
        return false;
    };
    let mut words = command.split_whitespace();
    let Some(executable) = words.next() else {
        return false;
    };
    if python_interpreter(executable) {
        return true;
    }
    if Path::new(executable)
        .file_name()
        .and_then(|name| name.to_str())
        != Some("env")
    {
        return false;
    }
    while let Some(word) = words.next() {
        if matches!(word, "-u" | "--unset" | "-C" | "--chdir" | "-a" | "--argv0") {
            words.next();
            continue;
        }
        if word.starts_with('-') || word.contains('=') {
            continue;
        }
        return python_interpreter(word);
    }
    false
}

fn python_crate(name: &str) -> bool {
    let name = name.replace('_', "-").to_ascii_lowercase();
    matches!(
        name.as_str(),
        "pyo3"
            | "rustpython"
            | "cpython"
            | "python3-sys"
            | "python27-sys"
            | "python3-dll-a"
            | "inline-python"
            | "pythonize"
    ) || name.starts_with("pyo3-")
        || name.starts_with("rustpython-")
}
fn python_node_package(name: &str) -> bool {
    matches!(
        name,
        "python-shell" | "python-bridge" | "node-python" | "python-node" | "pyodide"
    ) || name.starts_with("@pyodide/")
}
fn cargo_dependencies(table: &toml::map::Map<String, toml::Value>) -> Result<()> {
    for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(dependencies) = table.get(section).and_then(toml::Value::as_table) {
            for (alias, value) in dependencies {
                let actual = value
                    .get("package")
                    .and_then(toml::Value::as_str)
                    .unwrap_or(alias);
                ensure!(
                    !python_crate(actual) && !python_crate(alias),
                    "Python binding/runtime dependency is forbidden: {alias} ({actual})"
                );
            }
        }
    }
    Ok(())
}
fn check_manifest(name: &str, text: &str) -> Result<()> {
    if name == "package.json" {
        let manifest: serde_json::Value =
            serde_json::from_str(text).context("parse package.json")?;
        for section in [
            "dependencies",
            "devDependencies",
            "optionalDependencies",
            "peerDependencies",
        ] {
            if let Some(dependencies) = manifest.get(section).and_then(serde_json::Value::as_object)
            {
                for (name, version) in dependencies {
                    let alias = version
                        .as_str()
                        .and_then(|version| version.strip_prefix("npm:"));
                    let actual = alias.map_or(name.as_str(), |value| {
                        value
                            .rfind('@')
                            .filter(|index| *index > 0)
                            .map_or(value, |index| &value[..index])
                    });
                    ensure!(
                        !python_node_package(name) && !python_node_package(actual),
                        "Python runtime dependency is forbidden: {name} ({actual})"
                    );
                }
            }
        }
        return Ok(());
    }
    let manifest: toml::Value = toml::from_str(text).context("parse Cargo manifest/lockfile")?;
    if name == "cargo.lock" {
        for package in manifest
            .get("package")
            .and_then(toml::Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(name) = package.get("name").and_then(toml::Value::as_str) {
                ensure!(
                    !python_crate(name),
                    "resolved Python binding/runtime crate is forbidden: {name}"
                );
            }
        }
        return Ok(());
    }
    let root = manifest
        .as_table()
        .context("Cargo manifest must be a table")?;
    cargo_dependencies(root)?;
    if let Some(workspace) = root.get("workspace").and_then(toml::Value::as_table) {
        cargo_dependencies(workspace)?;
    }
    if let Some(targets) = root.get("target").and_then(toml::Value::as_table) {
        for target in targets.values().filter_map(toml::Value::as_table) {
            cargo_dependencies(target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn rejects_python_sources_artifacts_and_environment_manifests() {
        for path in [
            "code.PY",
            "typing.pyi",
            "launch.pyw",
            "a.pyc",
            "a.pyo",
            "runtime.pyd",
            "app.pyz",
            "book.ipynb",
            "pkg.whl",
            "pkg.egg",
            "nested/pyproject.toml",
            "Pipfile.lock",
            "uv.lock",
            "poetry.lock",
            "requirements-dev.txt",
            "requirements/base.in",
            "__pycache__/cache",
            ".python-version",
        ] {
            assert!(forbidden_path(Path::new(path)), "accepted {path}");
        }
        for path in [
            "README.md",
            "python-design.md",
            "requirements.md",
            "src/python_example.rs",
            "requirements/customer.txt.md",
            "Cargo.toml",
            "Cargo.lock",
            "package.json",
        ] {
            assert!(!forbidden_path(Path::new(path)), "rejected {path}");
        }
    }
    #[test]
    fn rejects_actual_python_interpreters_without_rejecting_arguments_or_prose() {
        for first_line in [
            "#!/usr/bin/python3\n",
            "#! /usr/bin/env python3.12 -u\n",
            "#!/usr/bin/env -S 'python3 -u'\n",
            "#!/usr/bin/env -u FOO MODE=x pypy3\n",
            "#!/usr/bin/env --unset=FOO python3.13t\n",
            "#!/usr/bin/env py\n",
        ] {
            assert!(
                python_shebang(first_line.as_bytes()),
                "accepted {first_line}"
            );
        }
        for text in [
            "Python is forbidden in this repository.\n",
            "#!/bin/sh # python3\n",
            "#!/usr/bin/env node python3\n",
            "#!/usr/bin/env python-language-server\n",
            "#!/usr/bin/env -u python3 node\n",
        ] {
            assert!(!python_shebang(text.as_bytes()), "rejected {text}");
        }
    }
    #[test]
    fn parses_dependency_aliases_targets_and_resolved_transitive_crates() {
        for manifest in [
            "[dependencies]\npyo3='1'",
            "[workspace.dependencies]\nbridge={package='pyo3',version='1'}",
            "[target.'cfg(windows)'.build-dependencies]\npython3-sys='1'",
            "[dev-dependencies]\nrustpython-vm='1'",
        ] {
            assert!(
                check_manifest("cargo.toml", manifest).is_err(),
                "accepted {manifest}"
            );
        }
        assert!(
            check_manifest(
                "cargo.lock",
                "version=4\n[[package]]\nname='pyo3-ffi'\nversion='1.0.0'"
            )
            .is_err()
        );
        assert!(check_manifest("cargo.toml", "[package]\nname='ordinary'\ndescription='Compared with Python and pyo3.'\n[dependencies]\nserde='1'").is_ok());
        assert!(
            check_manifest(
                "cargo.lock",
                "version=4\n[[package]]\nname='ordinary'\nversion='1.0.0'"
            )
            .is_ok()
        );
        assert!(
            check_manifest(
                "package.json",
                r#"{"dependencies":{"bridge":"npm:python-shell@5"}}"#
            )
            .is_err()
        );
        assert!(check_manifest("package.json", r#"{"devDependencies":{"pyodide":"1"}}"#).is_err());
        assert!(
            check_manifest(
                "package.json",
                r#"{"dependencies":{"bridge":"npm:@pyodide/runtime"}}"#
            )
            .is_err()
        );
        assert!(check_manifest("package.json", r#"{"description":"Python and pyodide","dependencies":{"node-gyp":"1","svelte":"5"}}"#).is_ok());
    }

    struct GitFixture(std::path::PathBuf);
    impl GitFixture {
        fn new() -> Self {
            let suffix = SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("loom-no-python-{}-{suffix}", std::process::id()));
            fs::create_dir(&path).expect("create owned fixture directory");
            let fixture = Self(path);
            fixture.git(&["init", "--quiet"]);
            fixture
        }
        fn git(&self, arguments: &[&str]) {
            let output = Command::new("git")
                .args(arguments)
                .current_dir(&self.0)
                .output()
                .expect("run git");
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
    impl Drop for GitFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn checks_git_owned_files_and_ignores_external_tool_caches() {
        let fixture = GitFixture::new();
        fs::write(fixture.0.join(".gitignore"), "node_modules/\ntarget/\n").unwrap();
        fs::write(
            fixture.0.join("README.md"),
            "Python and pyo3 are prohibited.\n",
        )
        .unwrap();
        fs::create_dir_all(fixture.0.join("node_modules/tool")).unwrap();
        fs::write(
            fixture.0.join("node_modules/tool/build.py"),
            "fixture bytes; never executed\n",
        )
        .unwrap();
        fixture.git(&["add", ".gitignore", "README.md"]);
        check(&fixture.0).expect("ignored dependencies do not become project source");
        fixture.git(&["add", "--force", "node_modules/tool/build.py"]);
        assert!(
            check(&fixture.0)
                .unwrap_err()
                .to_string()
                .contains("Python file is forbidden")
        );
    }
    #[test]
    fn detects_extensionless_python_and_aliased_runtime_in_tracked_files() {
        let fixture = GitFixture::new();
        fs::write(fixture.0.join("runner"), "#!/usr/bin/env -S python3 -u\n").unwrap();
        fixture.git(&["add", "runner"]);
        assert!(
            check(&fixture.0)
                .unwrap_err()
                .to_string()
                .contains("Python shebang")
        );
        fs::write(fixture.0.join("runner"), "#!/bin/sh\n").unwrap();
        fs::write(
            fixture.0.join("Cargo.toml"),
            "[dependencies]\nbridge={package='cpython',version='1'}\n",
        )
        .unwrap();
        fixture.git(&["add", "Cargo.toml"]);
        assert!(format!("{:#}", check(&fixture.0).unwrap_err()).contains("cpython"));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_extensionless_symlinks_to_python_files() {
        let fixture = GitFixture::new();
        std::os::unix::fs::symlink("outside/script.py", fixture.0.join("runner")).unwrap();
        fixture.git(&["add", "runner"]);
        assert!(
            check(&fixture.0)
                .unwrap_err()
                .to_string()
                .contains("Python symlink target")
        );
    }
}
