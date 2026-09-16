//! A small, hash-locked patch queue against the same immutable upstream pin.
use super::{DIRECTORY, Import, checked, digest, hexadecimal, inventory, relative, source};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs,
    path::Path,
    process::{Command, Output},
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Patch {
    path: String,
    sha256: String,
    purpose: String,
    files: Vec<File>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct File {
    path: String,
    before_sha256: String,
    after_sha256: String,
}

pub(super) fn verify(root: &Path, import: &Import) -> Result<()> {
    let directory = root.join(DIRECTORY).join("patches");
    let mut actual = BTreeSet::new();
    if directory.exists() {
        inventory(&directory, &directory, &mut actual)?;
    }
    let mut expected = BTreeSet::new();
    for patch in &import.patches {
        relative(&patch.path)?;
        ensure!(expected.insert(patch.path.clone()), "duplicate OMP² patch");
        ensure!(
            hexadecimal(&patch.sha256, 64) && !patch.purpose.trim().is_empty(),
            "OMP² patch requires a digest and purpose"
        );
        let path = directory.join(&patch.path);
        ensure!(
            fs::symlink_metadata(&path)?.is_file(),
            "OMP² patch must be a regular file"
        );
        ensure!(
            digest(&fs::read(path)?) == patch.sha256,
            "OMP² patch changed without a reviewed manifest: {}",
            patch.path
        );
        ensure!(!patch.files.is_empty(), "OMP² patch has no target files");
        let mut files = BTreeSet::new();
        for file in &patch.files {
            relative(&file.path)?;
            ensure!(
                !file
                    .path
                    .split('/')
                    .any(|part| part.eq_ignore_ascii_case(".git")),
                "OMP² patches cannot alter Git metadata"
            );
            ensure!(files.insert(&file.path), "duplicate OMP² patch target");
            ensure!(
                hexadecimal(&file.before_sha256, 64) && hexadecimal(&file.after_sha256, 64),
                "OMP² patch target requires before and after digests"
            );
        }
    }
    ensure!(
        actual == expected,
        "OMP² patch inventory differs from its manifest"
    );
    Ok(())
}

// Fetch from the explicitly supplied local object database into a fresh repository.
// No upstream hooks, submodules, build scripts or application code are executed.
fn checkout(source_root: &Path, revision: &str, output: &Path) -> Result<()> {
    ensure!(
        !output.exists(),
        "OMP² output already exists; use a fresh directory"
    );
    let source_root = source_root.canonicalize()?;
    // Rust's canonical Windows paths use a verbatim prefix that Git can
    // misread as an SSH host. A file URL also escapes spaces and URL delimiters.
    let source_url = url::Url::from_directory_path(&source_root)
        .map_err(|()| anyhow::anyhow!("cannot represent the local checkout as a file URL"))?;
    checked(
        &source_root,
        &["cat-file", "-e", &format!("{revision}^{{commit}}")],
    )?;
    fs::create_dir(output)?;
    checked(output, &["init", "-q", "--template="])?;
    checked(output, &["config", "core.autocrlf", "false"])?;
    // Use an empty directory rather than a platform-specific /dev/null hook path.
    fs::create_dir(output.join(".git/disabled-hooks"))?;
    checked(output, &["config", "core.hooksPath", ".git/disabled-hooks"])?;
    checked(
        output,
        &[
            "-c",
            "protocol.file.allow=always",
            "fetch",
            "--quiet",
            "--no-tags",
            "--depth=1",
            "--",
            source_url.as_str(),
            revision,
        ],
    )?;
    checked(output, &["checkout", "--quiet", "--detach", revision])?;
    Ok(())
}

fn check_file(root: &Path, file: &File, expected: &str) -> Result<()> {
    let path = root.join(&file.path);
    ensure!(
        fs::symlink_metadata(&path)?.is_file(),
        "patched source must be a regular file"
    );
    ensure!(
        digest(&fs::read(path)?) == expected,
        "OMP² patch target hash mismatch: {}",
        file.path
    );
    Ok(())
}

fn apply_command(output: &Path, patch: &Path, args: &[&str]) -> Result<Output> {
    // Let Rust open the file: Git's patch reader cannot handle canonical
    // Windows verbatim paths. File-backed stdin needs no pipe or temporary copy.
    Command::new("git")
        .arg("apply")
        .args(args)
        .arg("-")
        .current_dir(output)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(fs::File::open(patch).context("open OMP² patch")?)
        .output()
        .context("execute git apply for OMP² review")
}

fn apply(root: &Path, output: &Path, patch: &Patch) -> Result<()> {
    let path = root.join(DIRECTORY).join("patches").join(&patch.path);
    for args in [&["--check", "--index"][..], &["--index"][..]] {
        let result = apply_command(output, &path, args)?;
        ensure!(
            result.status.success(),
            "git apply failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    Ok(())
}

pub(super) fn prepare(root: &Path, source_root: &Path, output: &Path) -> Result<()> {
    super::verify(root)?;
    let import = super::load(root)?;
    for file in &import.files {
        ensure!(
            digest(&source(source_root, &import.revision, &file.path)?) == file.sha256,
            "upstream checkout disagrees with the pinned snapshot: {}",
            file.path
        );
    }
    ensure!(
        !output.exists(),
        "OMP² output already exists; use a fresh directory"
    );
    let result = (|| {
        checkout(source_root, &import.revision, output)?;
        let mut expected = BTreeSet::new();
        for patch in &import.patches {
            for file in &patch.files {
                check_file(output, file, &file.before_sha256)?;
                expected.insert(file.path.as_str());
            }
            apply(root, output, patch)?;
            for file in &patch.files {
                check_file(output, file, &file.after_sha256)?;
            }
        }
        let changed = checked(
            output,
            &["diff", "--cached", "--name-only", "-z", "HEAD", "--"],
        )?;
        let actual = std::str::from_utf8(&changed)?
            .split('\0')
            .filter(|p| !p.is_empty())
            .collect::<BTreeSet<_>>();
        ensure!(
            actual == expected,
            "OMP² patch touched unlisted files or omitted listed changes"
        );
        checked(output, &["diff", "--cached", "--check"])?;
        Ok(())
    })();
    if result.is_err() && output.exists() {
        // Only the new directory owned by this invocation is removed on failure.
        fs::remove_dir_all(output).context("remove incomplete OMP² prepared checkout")?;
    }
    result?;
    println!(
        "OMP² patched checkout: {} ({} patches at {})",
        output.display(),
        import.patches.len(),
        import.revision
    );
    Ok(())
}

pub(super) fn review(
    root: &Path,
    source_root: &Path,
    revision: &str,
    output: &Path,
    import: &Import,
) -> Result<String> {
    if import.patches.is_empty() {
        return Ok(String::new());
    }
    let scratch = output.join("patch-check");
    let result = (|| {
        checkout(source_root, revision, &scratch)?;
        let mut report =
            String::from("Maintained runtime patch queue (no upstream code executed):\n");
        for patch in &import.patches {
            let path = root.join(DIRECTORY).join("patches").join(&patch.path);
            if apply_command(&scratch, &path, &["--check", "--index"])?
                .status
                .success()
            {
                apply(root, &scratch, patch)?;
                report.push_str(&format!(
                    "  {}: applies; review changed baselines and rerun the runtime gate.\n",
                    patch.path
                ));
            } else if apply_command(&scratch, &path, &["--reverse", "--check", "--index"])?
                .status
                .success()
            {
                report.push_str(&format!(
                    "  {}: exact changes already present; test before retiring this patch.\n",
                    patch.path
                ));
            } else {
                report.push_str(&format!(
                    "  {}: CONFLICT; rebase explicitly, then check the remaining series.\n",
                    patch.path
                ));
                break;
            }
        }
        report.push_str("Pin promotion requires refreshed before/after hashes and passing tests on the resulting patched tree. Never bypass a patch conflict or infer semantic equivalence from a clean apply.\n");
        Ok(report)
    })();
    if scratch.exists() {
        fs::remove_dir_all(&scratch)?;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::super::tests::{command, setup};
    use super::*;

    fn add_patch(upstream: &Path, downstream: &Path) {
        fs::write(upstream.join("codec.rs"), "fixed behavior\n").expect("fixture");
        let patch = checked(
            upstream,
            &[
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--binary",
                "HEAD",
                "--",
            ],
        )
        .expect("fixture");
        let mut import = super::super::load(downstream).expect("fixture");
        import.patches = vec![Patch {
            path: "runtime.patch".into(),
            sha256: digest(&patch),
            purpose: "runtime regression".into(),
            files: vec![File {
                path: "codec.rs".into(),
                before_sha256: digest(b"old behavior\n"),
                after_sha256: digest(b"fixed behavior\n"),
            }],
        }];
        let directory = downstream.join(DIRECTORY).join("patches");
        fs::create_dir_all(&directory).expect("fixture");
        fs::write(directory.join("runtime.patch"), patch).expect("fixture");
        fs::write(
            downstream.join(super::super::LOCK),
            serde_json::to_vec_pretty(&import).expect("fixture"),
        )
        .expect("fixture");
        command(upstream, &["restore", "--", "codec.rs"]);
    }

    #[test]
    fn prepares_exact_patch_and_preserves_the_source_checkout() {
        let (fixture, upstream, downstream, _) = setup();
        add_patch(&upstream, &downstream);
        fs::write(upstream.join("codec.rs"), "uncommitted user work\n").expect("fixture");
        let output = fixture.0.join("prepared");
        prepare(&downstream, &upstream, &output).expect("prepare");
        assert_eq!(
            fs::read_to_string(output.join("codec.rs")).expect("fixture"),
            "fixed behavior\n"
        );
        assert_eq!(
            fs::read_to_string(upstream.join("codec.rs")).expect("fixture"),
            "uncommitted user work\n"
        );
        assert!(prepare(&downstream, &upstream, &output).is_err());
        let patch = downstream.join(DIRECTORY).join("patches/runtime.patch");
        command(
            &output,
            &[
                "apply",
                "--reverse",
                "--index",
                patch.to_str().expect("fixture"),
            ],
        );
        assert_eq!(command(&output, &["diff", "--cached", "HEAD"]), "");
    }

    #[test]
    fn wrong_baseline_or_patched_hash_cannot_produce_a_checkout() {
        let (fixture, upstream, downstream, _) = setup();
        add_patch(&upstream, &downstream);
        for before in [true, false] {
            let mut import = super::super::load(&downstream).expect("fixture");
            let file = &mut import.patches[0].files[0];
            if before {
                file.before_sha256 = "0".repeat(64);
            } else {
                file.before_sha256 = digest(b"old behavior\n");
                file.after_sha256 = "0".repeat(64);
            }
            fs::write(
                downstream.join(super::super::LOCK),
                serde_json::to_vec_pretty(&import).expect("fixture"),
            )
            .expect("fixture");
            let output = fixture.0.join("failed");
            assert!(prepare(&downstream, &upstream, &output).is_err());
            assert!(!output.exists());
        }
    }

    #[test]
    fn patch_tampering_and_unlisted_patches_are_rejected() {
        let (_fixture, upstream, downstream, _) = setup();
        add_patch(&upstream, &downstream);
        let directory = downstream.join(DIRECTORY).join("patches");
        let path = directory.join("runtime.patch");
        let original = fs::read(&path).expect("fixture");
        fs::write(&path, "tampered\n").expect("fixture");
        assert!(super::super::verify(&downstream).is_err());
        fs::write(&path, original).expect("fixture");
        fs::write(directory.join("unlisted.patch"), "extra\n").expect("fixture");
        assert!(super::super::verify(&downstream).is_err());
    }

    #[test]
    fn patch_target_inventory_cannot_hide_an_extra_change() {
        let (fixture, upstream, downstream, _) = setup();
        add_patch(&upstream, &downstream);
        fs::write(upstream.join("LICENSE"), "changed license\n").expect("fixture");
        let extra = checked(
            &upstream,
            &["diff", "--no-ext-diff", "--binary", "HEAD", "--", "LICENSE"],
        )
        .expect("fixture");
        let path = downstream.join(DIRECTORY).join("patches/runtime.patch");
        let mut bytes = fs::read(&path).expect("fixture");
        bytes.extend(extra);
        fs::write(&path, &bytes).expect("fixture");
        let mut import = super::super::load(&downstream).expect("fixture");
        import.patches[0].sha256 = digest(&bytes);
        fs::write(
            downstream.join(super::super::LOCK),
            serde_json::to_vec_pretty(&import).expect("fixture"),
        )
        .expect("fixture");
        let output = fixture.0.join("failed");
        assert!(prepare(&downstream, &upstream, &output).is_err());
        assert!(!output.exists());
    }

    #[test]
    fn review_distinguishes_applicable_absorbed_and_conflicting_patches() {
        let (fixture, upstream, downstream, baseline) = setup();
        add_patch(&upstream, &downstream);
        let import = super::super::load(&downstream).expect("fixture");
        assert!(
            review(&downstream, &upstream, &baseline, &fixture.0, &import)
                .expect("review")
                .contains("applies;")
        );
        fs::write(upstream.join("codec.rs"), "fixed behavior\n").expect("fixture");
        command(&upstream, &["commit", "-qam", "upstream fix"]);
        let revision = command(&upstream, &["rev-parse", "HEAD"]);
        assert!(
            review(&downstream, &upstream, &revision, &fixture.0, &import)
                .expect("review")
                .contains("already present")
        );
        fs::write(upstream.join("codec.rs"), "different behavior\n").expect("fixture");
        command(&upstream, &["commit", "-qam", "upstream rewrite"]);
        let revision = command(&upstream, &["rev-parse", "HEAD"]);
        assert!(
            review(&downstream, &upstream, &revision, &fixture.0, &import)
                .expect("review")
                .contains("CONFLICT")
        );
    }
}
