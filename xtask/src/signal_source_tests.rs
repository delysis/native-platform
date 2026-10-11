use super::*;

fn write(root: &Path, path: &str, contents: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().expect("file parent")).expect("create parent");
    fs::write(path, contents).expect("write fixture");
}

fn commit(root: &Path) {
    checked(Command::new("git").current_dir(root).args(["add", "."])).expect("stage fixture");
    checked(Command::new("git").current_dir(root).args([
        "-c",
        "user.name=Source test",
        "-c",
        "user.email=source@example.invalid",
        "-c",
        "commit.gpgsign=false",
        "commit",
        "-m",
        "fixture",
    ]))
    .expect("commit fixture");
}

fn repository(root: &Path) {
    fs::create_dir(root).expect("create fixture repository");
    checked(
        Command::new("git")
            .current_dir(root)
            .args(["init", "--initial-branch=main"]),
    )
    .expect("initialize fixture repository");
}

#[test]
fn libsignal_notice_verification_rejects_changed_text_or_a_different_dependency_pin() {
    let temp = tempfile::tempdir().expect("notice fixture");
    let directory = temp.path().join("notices/libsignal");
    write(&directory, "LICENSE", "fixture license\n");
    write(
        &directory,
        "acknowledgments/acknowledgments-desktop.md",
        "fixture acknowledgments\n",
    );
    let source = "git+https://github.com/signalapp/libsignal?tag=v0.99.0#reviewed";
    let receipt = serde_json::json!({"source": source, "files": {
        "LICENSE": hash_file(&directory.join("LICENSE")).expect("license hash"),
        "acknowledgments/acknowledgments-desktop.md": hash_file(&directory.join("acknowledgments/acknowledgments-desktop.md")).expect("acknowledgments hash")
    }});
    write(&directory, "UPSTREAM.json", &receipt.to_string());
    let mut packages = [Package {
        name: "libsignal-core".into(),
        version: "0.1.0".into(),
        source: Some(source.into()),
        license: Some("AGPL-3.0-only".into()),
        license_file: None,
    }];
    verify_git_notices(temp.path(), &packages).expect("reviewed notice");
    write(&directory, "LICENSE", "changed license\n");
    assert!(verify_git_notices(temp.path(), &packages).is_err());
    write(&directory, "LICENSE", "fixture license\n");
    packages[0].source =
        Some("git+https://github.com/signalapp/libsignal?tag=new#unreviewed".into());
    assert!(verify_git_notices(temp.path(), &packages).is_err());
}

#[test]
fn archived_git_and_path_dependencies_build_without_original_sources_or_cargo_cache() {
    let temp = tempfile::tempdir().expect("fixture directory");
    let upstream = temp.path().join("upstream");
    repository(&upstream);
    write(
        &upstream,
        "Cargo.toml",
        "[package]\nname='source-test-dependency'\nversion='0.1.0'\nedition='2024'\nlicense='MIT'\n",
    );
    write(&upstream, "src/lib.rs", "pub const ANSWER: u8 = 42;\n");
    write(&upstream, "LICENSE", "fixture license\n");
    commit(&upstream);
    let revision = git_id(&upstream, "HEAD").expect("dependency revision");
    let root = temp.path().join("project");
    repository(&root);
    write(&root, ".gitignore", "/private/\n");
    write(
        &root,
        "private/account-key",
        "private fixture, never distribute\n",
    );
    write(
        &root,
        "products/loom/crates/helper/Cargo.toml",
        "[package]\nname='source-test-helper'\nversion='0.1.0'\nedition='2024'\n",
    );
    write(
        &root,
        "products/loom/crates/helper/src/lib.rs",
        "pub const EXPECTED: u8 = 42;\n",
    );
    write(
        &root,
        &format!("{WORKER}/Cargo.toml"),
        &format!(
            "[workspace]\n[package]\nname='source-test-worker'\nversion='0.1.0'\nedition='2024'\n[dependencies]\nsource-test-helper={{path='../crates/helper'}}\nsource-test-dependency={{git='file://{}',rev='{revision}'}}\n",
            upstream.display()
        ),
    );
    write(
        &root,
        &format!("{WORKER}/src/main.rs"),
        "fn main() { assert_eq!(source_test_dependency::ANSWER, source_test_helper::EXPECTED); }\n",
    );
    checked(
        cargo()
            .current_dir(root.join(WORKER))
            .arg("generate-lockfile"),
    )
    .expect("resolve fixture dependency");
    commit(&root);

    let output = temp.path().join("release");
    let receipt = assemble(&root, &output).expect("assemble source distribution");
    assert_eq!(receipt.packages.len(), 3);
    assert_eq!(
        receipt.source.revision,
        git_id(&root, "HEAD").expect("source revision")
    );
    let archive = output.join(&receipt.archive);
    assert_eq!(
        hash_file(&archive).expect("archive hash"),
        receipt.archive_sha256
    );
    assert!(
        assemble(&root, &output).is_err(),
        "cannot replace an existing distribution"
    );
    write(&root, &format!("{WORKER}/src/main.rs"), "fn main() {}\n");
    assert!(
        assemble(&root, &temp.path().join("dirty-release")).is_err(),
        "cannot archive dirty source"
    );
    assert!(!temp.path().join("dirty-release").exists());

    // Remove the original repositories and use a fresh Cargo home. A metadata-only
    // success cannot mask missing build inputs from the vendored Git dependency.
    fs::remove_dir_all(&upstream).expect("remove fixture upstream");
    fs::remove_dir_all(&root).expect("remove fixture source");
    let extracted = temp.path().join("extracted");
    fs::create_dir(&extracted).expect("extraction directory");
    checked(
        Command::new("tar")
            .arg("-xzf")
            .arg(&archive)
            .arg("-C")
            .arg(&extracted),
    )
    .expect("extract distributed artifact");
    let extracted = extracted.join(format!("loom-signal-source-{}", receipt.source.revision));
    assert!(!extracted.join("private").exists());
    assert!(!extracted.join(".git").exists());
    assert_eq!(
        fs::read_to_string(
            extracted
                .join(WORKER)
                .join("vendor-cargo/source-test-dependency-0.1.0/LICENSE")
        )
        .expect("vendored notice"),
        "fixture license\n"
    );
    let cargo_home = tempfile::tempdir().expect("empty Cargo home");
    checked(
        cargo()
            .current_dir(extracted.join(WORKER))
            .env("CARGO_HOME", cargo_home.path())
            .env("CARGO_TARGET_DIR", temp.path().join("compiled"))
            .args(["run", "--frozen"]),
    )
    .expect("run archived sources without original sources or cached dependencies");
}
