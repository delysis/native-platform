//! Reviewable OMP² imports. Never executes upstream code or changes the live pin.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    path::{Component, Path},
    process::{Command, Output},
};

const DIRECTORY: &str = "third-party/omp2";
const LOCK: &str = "third-party/omp2/import.json";
const REPOSITORY: &str = "https://github.com/can1357/oh-my-pi.git";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Import {
    schema_version: u32,
    repository: String,
    branch: String,
    revision: String,
    files: Vec<Source>,
    watch_paths: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Source {
    path: String,
    sha256: String,
    consumers: Vec<String>,
    purpose: String,
}

pub fn run(root: &Path, args: &[String]) -> Result<()> {
    match args {
        [command] if command == "verify" => verify(root),
        [command, checkout, revision, output] if command == "review" => {
            review(root, Path::new(checkout), revision, Path::new(output))
        }
        _ => bail!(
            "usage: cargo run --locked -p xtask -- omp2 verify | omp2 review <checkout> <full-commit-sha> <new-output-directory>"
        ),
    }
}

fn hexadecimal(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn relative(path: &str) -> Result<()> {
    ensure!(
        !path.is_empty() && !path.contains('\\') && !path.contains('\n'),
        "invalid source path: {path}"
    );
    ensure!(
        Path::new(path)
            .components()
            .all(|c| matches!(c, Component::Normal(_))),
        "source path must be repository-relative: {path}"
    );
    Ok(())
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn load(root: &Path) -> Result<Import> {
    let import: Import = serde_json::from_slice(&fs::read(root.join(LOCK))?)
        .context("parse OMP² import manifest")?;
    ensure!(
        import.schema_version == 1,
        "unsupported OMP² manifest version"
    );
    ensure!(
        import.repository == REPOSITORY && import.branch == "omp2",
        "upstream repository or branch changed; review the importer before changing authority"
    );
    ensure!(
        hexadecimal(&import.revision, 40),
        "OMP² revision must be a full lowercase commit SHA"
    );
    ensure!(
        !import.files.is_empty() && !import.watch_paths.is_empty(),
        "empty OMP² import"
    );
    let mut paths = BTreeSet::new();
    for source in &import.files {
        relative(&source.path)?;
        ensure!(
            paths.insert(&source.path),
            "duplicate source: {}",
            source.path
        );
        ensure!(
            hexadecimal(&source.sha256, 64),
            "invalid digest for {}",
            source.path
        );
        ensure!(
            !source.purpose.trim().is_empty() && !source.consumers.is_empty(),
            "source without a consumer: {}",
            source.path
        );
        for consumer in &source.consumers {
            relative(consumer)?;
            ensure!(
                root.join(consumer).is_file(),
                "missing OMP² consumer: {consumer}"
            );
        }
    }
    ensure!(
        paths.contains(&"LICENSE".to_owned()),
        "OMP² license must accompany the import"
    );
    for path in &import.watch_paths {
        relative(path)?;
    }
    Ok(import)
}

pub fn verify(root: &Path) -> Result<()> {
    let import = load(root)?;
    let snapshot = root.join(DIRECTORY).join("upstream");
    let mut actual = BTreeSet::new();
    inventory(&snapshot, &snapshot, &mut actual)?;
    let expected = import
        .files
        .iter()
        .map(|f| f.path.clone())
        .collect::<BTreeSet<_>>();
    ensure!(
        actual == expected,
        "OMP² snapshot inventory differs from its manifest"
    );
    for source in &import.files {
        let bytes = fs::read(snapshot.join(&source.path))?;
        ensure!(
            digest(&bytes) == source.sha256,
            "OMP² snapshot changed without a reviewed pin: {}",
            source.path
        );
    }
    println!(
        "OMP² source verified: {} ({} files)",
        import.revision,
        import.files.len()
    );
    Ok(())
}

fn inventory(root: &Path, directory: &Path, paths: &mut BTreeSet<String>) -> Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        ensure!(
            !kind.is_symlink(),
            "symlinks are forbidden in the OMP² source snapshot"
        );
        if kind.is_dir() {
            inventory(root, &entry.path(), paths)?;
        } else {
            ensure!(kind.is_file(), "non-file OMP² source entry");
            paths.insert(
                entry
                    .path()
                    .strip_prefix(root)?
                    .to_str()
                    .context("non-UTF8 source path")?
                    .replace('\\', "/"),
            );
        }
    }
    Ok(())
}

fn git(checkout: &Path, args: &[&str]) -> Result<Output> {
    Command::new("git")
        .args(args)
        .current_dir(checkout)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .context("execute git for OMP² review")
}

fn checked(checkout: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = git(checkout, args)?;
    ensure!(
        output.status.success(),
        "git failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output.stdout)
}

fn source(checkout: &Path, revision: &str, path: &str) -> Result<Vec<u8>> {
    let object = format!("{revision}:{path}");
    let kind = checked(checkout, &["cat-file", "-t", &object])?;
    ensure!(kind == b"blob\n", "source is not a Git blob: {path}");
    checked(checkout, &["show", &object])
}

fn write(root: &Path, path: &Path, bytes: &[u8]) -> Result<()> {
    let destination = root.join(path);
    fs::create_dir_all(destination.parent().context("destination parent")?)?;
    fs::write(destination, bytes)?;
    Ok(())
}

fn review(root: &Path, checkout: &Path, revision: &str, output: &Path) -> Result<()> {
    verify(root)?;
    ensure!(
        hexadecimal(revision, 40),
        "candidate must be a full lowercase commit SHA"
    );
    ensure!(
        !output.exists(),
        "review directory already exists; use a fresh directory"
    );
    let import = load(root)?;
    let mut candidate = import.clone();
    candidate.revision = revision.to_owned();
    let commit = format!("{revision}^{{commit}}");
    checked(checkout, &["cat-file", "-e", &commit])?;
    let ancestor = git(
        checkout,
        &["merge-base", "--is-ancestor", &import.revision, revision],
    )?;
    ensure!(
        ancestor.status.success(),
        "candidate is not a proven descendant of the pinned commit; fetch full history or review an upstream rewrite separately"
    );

    // Read every object before creating output: deletion, shallow history and
    // baseline mismatch fail without leaving an apparently usable proposal.
    let mut blobs = Vec::new();
    for entry in &mut candidate.files {
        let original = source(checkout, &import.revision, &entry.path)?;
        ensure!(
            digest(&original) == entry.sha256,
            "pinned upstream object does not match checked-in source: {}",
            entry.path
        );
        let next = source(checkout, revision, &entry.path).with_context(|| {
            format!(
                "{} was deleted/renamed; adapt the import explicitly",
                entry.path
            )
        })?;
        entry.sha256 = digest(&next);
        blobs.push((entry.path.clone(), original, next));
    }
    let range = format!("{}..{revision}", import.revision);
    let mut args = vec![
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--binary",
        &range,
        "--",
    ];
    args.extend(import.watch_paths.iter().map(String::as_str));
    let upstream_diff = checked(checkout, &args)?;
    fs::create_dir(output)?;
    let baseline = output.join("baseline");
    let next = output.join("candidate");
    for (path, original, updated) in blobs {
        let destination = Path::new(DIRECTORY).join("upstream").join(path);
        write(&baseline, &destination, &original)?;
        write(&next, &destination, &updated)?;
    }
    write(&baseline, Path::new(LOCK), &fs::read(root.join(LOCK))?)?;
    let mut manifest = serde_json::to_vec_pretty(&candidate)?;
    manifest.push(b'\n');
    write(&next, Path::new(LOCK), &manifest)?;
    fs::write(output.join("upstream.diff"), upstream_diff)?;
    let patch = git(
        output,
        &[
            "diff",
            "--no-index",
            "--no-ext-diff",
            "--no-textconv",
            "--binary",
            "--",
            "baseline",
            "candidate",
        ],
    )?;
    ensure!(
        matches!(patch.status.code(), Some(0 | 1)),
        "could not construct import patch"
    );
    fs::write(output.join("import.patch"), patch.stdout)?;
    let summary = format!(
        "OMP² candidate {} -> {revision}\n\nReview upstream.diff, including changes outside the imported files.\nReview every consumer listed in candidate/{LOCK}.\nThe snapshot patch alone does not port changed behavior.\nAfter adapting consumers, from the native-platform workspace:\n  git apply --check -p2 <review-directory>/import.patch\n  git apply -p2 <review-directory>/import.patch\n  cargo run --locked -p xtask -- omp2 verify\nRun the gates in products/fte/docs/OMP2-MAINTENANCE.md before committing.\nNo live pin or consumer was changed by this command.\n",
        import.revision
    );
    fs::write(output.join("REVIEW.txt"), summary)?;
    println!("OMP² review bundle: {}", output.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicUsize, Ordering},
    };
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "omp2-import-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).expect("valid test fixture");
            Self(root)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn command(root: &Path, args: &[&str]) -> String {
        String::from_utf8(checked(root, args).expect("valid test fixture"))
            .expect("valid test fixture")
            .trim()
            .to_owned()
    }
    fn setup() -> (Fixture, PathBuf, PathBuf, String) {
        let fixture = Fixture::new();
        let upstream = fixture.0.join("upstream");
        let downstream = fixture.0.join("downstream");
        fs::create_dir(&upstream).expect("valid test fixture");
        fs::create_dir(&downstream).expect("valid test fixture");
        command(&upstream, &["init", "-q"]);
        command(&upstream, &["config", "user.name", "Import test"]);
        command(
            &upstream,
            &["config", "user.email", "import@example.invalid"],
        );
        fs::write(upstream.join("LICENSE"), "test license\n").expect("valid test fixture");
        fs::write(upstream.join("codec.rs"), "old behavior\n").expect("valid test fixture");
        command(&upstream, &["add", "."]);
        command(&upstream, &["commit", "-qm", "baseline"]);
        let revision = command(&upstream, &["rev-parse", "HEAD"]);
        fs::write(downstream.join("consumer.rs"), "local adaptation\n")
            .expect("valid test fixture");
        let files = ["LICENSE", "codec.rs"]
            .into_iter()
            .map(|path| {
                let bytes = fs::read(upstream.join(path)).expect("valid test fixture");
                write(
                    &downstream,
                    &Path::new(DIRECTORY).join("upstream").join(path),
                    &bytes,
                )
                .expect("valid test fixture");
                Source {
                    path: path.into(),
                    sha256: digest(&bytes),
                    consumers: vec!["consumer.rs".into()],
                    purpose: "test".into(),
                }
            })
            .collect();
        let import = Import {
            schema_version: 1,
            repository: REPOSITORY.into(),
            branch: "omp2".into(),
            revision: revision.clone(),
            files,
            watch_paths: vec!["codec.rs".into()],
        };
        write(
            &downstream,
            Path::new(LOCK),
            &serde_json::to_vec_pretty(&import).expect("valid test fixture"),
        )
        .expect("valid test fixture");
        command(&downstream, &["init", "-q"]);
        (fixture, upstream, downstream, revision)
    }

    #[test]
    fn proposal_applies_and_rollback_restores_verified_sources() {
        let (fixture, upstream, downstream, _) = setup();
        fs::write(upstream.join("codec.rs"), "improved behavior\n").expect("valid test fixture");
        command(&upstream, &["commit", "-qam", "improvement"]);
        let revision = command(&upstream, &["rev-parse", "HEAD"]);
        let output = fixture.0.join("review");
        let before = fs::read(downstream.join(LOCK)).expect("valid test fixture");
        review(&downstream, &upstream, &revision, &output).expect("valid test fixture");
        assert_eq!(
            before,
            fs::read(downstream.join(LOCK)).expect("valid test fixture")
        );
        let patch = output.join("import.patch");
        command(
            &downstream,
            &[
                "apply",
                "--check",
                "-p2",
                patch.to_str().expect("valid test fixture"),
            ],
        );
        command(
            &downstream,
            &["apply", "-p2", patch.to_str().expect("valid test fixture")],
        );
        verify(&downstream).expect("valid test fixture");
        assert_eq!(
            load(&downstream).expect("valid test fixture").revision,
            revision
        );
        assert_eq!(
            fs::read_to_string(downstream.join("consumer.rs")).expect("valid test fixture"),
            "local adaptation\n"
        );
        command(
            &downstream,
            &[
                "apply",
                "-R",
                "-p2",
                patch.to_str().expect("valid test fixture"),
            ],
        );
        verify(&downstream).expect("valid test fixture");
        assert_eq!(
            before,
            fs::read(downstream.join(LOCK)).expect("valid test fixture")
        );
    }

    #[test]
    fn tampering_and_untracked_source_are_rejected() {
        let (_fixture, _, downstream, _) = setup();
        let path = downstream.join(DIRECTORY).join("upstream/codec.rs");
        fs::write(&path, "tampered").expect("valid test fixture");
        assert!(verify(&downstream).is_err());
        fs::write(&path, "old behavior\n").expect("valid test fixture");
        fs::write(path.with_file_name("surprise.rs"), "extra").expect("valid test fixture");
        assert!(verify(&downstream).is_err());
    }

    #[test]
    fn deleted_source_rejects_without_creating_a_proposal() {
        let (fixture, upstream, downstream, _) = setup();
        command(&upstream, &["rm", "codec.rs"]);
        command(&upstream, &["commit", "-qm", "removed"]);
        let revision = command(&upstream, &["rev-parse", "HEAD"]);
        let output = fixture.0.join("review");
        assert!(review(&downstream, &upstream, &revision, &output).is_err());
        assert!(!output.exists());
    }

    #[test]
    fn revision_and_path_inputs_cannot_be_git_options_or_traversal() {
        for path in ["", "../secret", "/absolute", "a/../../b", "a\\b", "a\nb"] {
            assert!(relative(path).is_err());
        }
        for revision in ["main", "HEAD", "--help", "abcdef"] {
            assert!(!hexadecimal(revision, 40));
        }
    }

    #[test]
    fn upstream_snapshot_bytes_survive_crlf_enabled_checkout() {
        let (fixture, _, downstream, _) = setup();
        fs::write(
            downstream.join(".gitattributes"),
            include_bytes!("../../.gitattributes"),
        )
        .expect("copy real checkout attributes");
        let path = format!("{DIRECTORY}/upstream/codec.rs");
        command(&downstream, &["add", ".gitattributes", &path]);
        let output = fixture.0.join("crlf-checkout");
        let prefix = format!("--prefix={}/", output.display()).replace('\\', "/");
        command(
            &downstream,
            &[
                "-c",
                "core.autocrlf=true",
                "checkout-index",
                &prefix,
                "--",
                &path,
            ],
        );
        assert_eq!(
            fs::read(output.join(&path)).expect("checked-out source"),
            fs::read(downstream.join(path)).expect("original source")
        );
    }
}
