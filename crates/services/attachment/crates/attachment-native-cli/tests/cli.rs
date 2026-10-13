#![forbid(unsafe_code)]

use attachment_native_host::AttachmentHostConfig;
use serde_json::{Value, json};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const MAX_CONFIG_BYTES: usize = 1024 * 1024;
static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

#[test]
fn inspect_emits_machine_readable_graph_artifacts_and_receipt() {
    let directory = TestDirectory::new("inspect-json");
    let attachment = directory.write("note.md", b"# Local note\n\nSafe attachment text.\n");
    let output = run(["inspect".as_ref(), attachment.as_os_str()]);

    assert_success(&output);
    let value = output_json(&output);
    assert_eq!(value["graph"]["schema"], "attachment_native.graph.v1");
    assert_eq!(
        value["graph"]["objects"][0]["detection"]["selected"],
        "markdown"
    );
    assert!(
        value["artifacts"]
            .as_array()
            .is_some_and(|items| !items.is_empty())
    );
    assert_eq!(value["receipt"]["network_used"], false);
    assert_eq!(value["receipt"]["process_used"], false);
    assert_eq!(value["receipt"]["model_invoked"], false);
}

#[test]
fn plan_emits_a_valid_preparation_contract_for_an_exact_target() {
    let directory = TestDirectory::new("plan-json");
    let attachment = directory.write("note.md", b"# Context\n\nUse this as untrusted context.\n");
    let target = directory.write_json(
        "target.json",
        &json!({
            "target_id": "cli-fixture",
            "fingerprint": "cli-fixture:v1",
            "accepted_media_types": [],
            "accepted_media_families": [],
            "max_media_objects": 4,
            "max_media_bytes": 1048576,
            "max_text_bytes": 1048576,
            "supports_markdown": true,
            "supports_native_pdf": false,
            "supports_native_video": false
        }),
    );
    let output = run([
        "plan".as_ref(),
        attachment.as_os_str(),
        "--target".as_ref(),
        target.as_os_str(),
    ]);

    assert_success(&output);
    let value = output_json(&output);
    assert_eq!(
        value["plan"]["schema"],
        "attachment_native.preparation_plan.v2"
    );
    assert_eq!(value["plan"]["target_id"], "cli-fixture");
    assert!(
        value["plan"]["parts"]
            .as_array()
            .is_some_and(|items| !items.is_empty())
    );
    assert_eq!(value["receipt"]["network_used"], false);
}

#[test]
fn oversized_configuration_is_rejected_before_processing() {
    let directory = TestDirectory::new("oversized-config");
    let attachment = directory.write("note.txt", b"small attachment");
    let oversized = vec![b' '; MAX_CONFIG_BYTES + 1];
    let config = directory.write("oversized.json", &oversized);
    let output = run([
        "inspect".as_ref(),
        attachment.as_os_str(),
        "--config".as_ref(),
        config.as_os_str(),
    ]);

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(stderr(&output).contains("exceeds the 1048576-byte configuration limit"));
}

#[test]
fn zero_parser_and_decoder_limits_are_rejected_as_invalid_policy() {
    let directory = TestDirectory::new("invalid-parser-limits");
    let attachment = directory.write("note.txt", b"small attachment");
    for (name, configure) in [
        (
            "parser.json",
            zero_parser_limit as fn(&mut AttachmentHostConfig),
        ),
        (
            "decoder.json",
            zero_decoder_limit as fn(&mut AttachmentHostConfig),
        ),
    ] {
        let mut config = AttachmentHostConfig::default();
        configure(&mut config);
        let config = directory.write_json(
            name,
            &serde_json::to_value(config).expect("host configuration must encode"),
        );
        let output = run([
            "inspect".as_ref(),
            attachment.as_os_str(),
            "--config".as_ref(),
            config.as_os_str(),
        ]);

        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(
            stderr(&output).contains("inspection_policy_invalid"),
            "unexpected policy rejection: {}",
            stderr(&output)
        );
    }
}

#[test]
fn directory_input_is_rejected_as_a_non_file() {
    let directory = TestDirectory::new("non-file");
    let output = run(["inspect".as_ref(), directory.path().as_os_str()]);

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let message = stderr(&output);
    assert!(
        message.contains("not a regular file") || message.contains("failed to open attachment"),
        "unexpected non-file rejection: {message}"
    );
}

#[test]
fn bytes_override_a_misleading_name_and_declared_media_type() {
    let directory = TestDirectory::new("content-first");
    let attachment = directory.write("misleading.pdf", br#"{"safe":true,"source":"bytes"}"#);
    let output = run([
        "inspect".as_ref(),
        attachment.as_os_str(),
        "--media-type".as_ref(),
        "application/pdf".as_ref(),
    ]);

    assert_success(&output);
    let value = output_json(&output);
    assert_eq!(
        value["graph"]["objects"][0]["detection"]["selected"],
        "json"
    );
    assert_ne!(value["graph"]["objects"][0]["detection"]["selected"], "pdf");
    assert!(value["graph"]["issues"].as_array().is_some_and(|issues| {
        issues
            .iter()
            .any(|issue| issue["code"] == "declared_type_mismatch")
    }));
}

fn run<I, S>(arguments: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Command::new(env!("CARGO_BIN_EXE_attachment-native"))
        .args(arguments)
        .output()
        .expect("CLI process must start")
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "CLI failed with {}: {}",
        output.status,
        stderr(output)
    );
}

fn output_json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).expect("CLI stdout must be valid JSON")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn zero_parser_limit(config: &mut AttachmentHostConfig) {
    config.inspection.limits.max_parser_input_bytes = 0;
}

fn zero_decoder_limit(config: &mut AttachmentHostConfig) {
    config.inspection.limits.max_decoder_window_bytes = 0;
}

struct TestDirectory {
    path: PathBuf,
}

impl TestDirectory {
    fn new(label: &str) -> Self {
        let serial = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "attachment-native-cli-{label}-{}-{serial}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("test directory must be created");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.path.join(name);
        fs::write(&path, bytes).expect("test fixture must be written");
        path
    }

    fn write_json(&self, name: &str, value: &Value) -> PathBuf {
        let bytes = serde_json::to_vec(value).expect("JSON fixture must encode");
        self.write(name, &bytes)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[cfg(unix)]
mod fifo_admission {
    use super::{TestDirectory, assert_success, output_json, run, stderr};
    use attachment_native_host::AttachmentHostConfig;
    use nix::sys::stat::Mode;
    use serde_json::json;
    use std::ffi::OsString;
    use std::fs::{self, File};
    use std::process::{Child, Command, Output, Stdio};
    use std::time::{Duration, Instant};

    #[derive(Clone, Copy, Debug)]
    enum FileRole {
        InspectAttachment,
        PlanAttachment,
        InspectConfiguration,
        PlanTarget,
    }

    impl FileRole {
        fn label(self) -> &'static str {
            match self {
                Self::InspectAttachment => "inspect-attachment",
                Self::PlanAttachment => "plan-attachment",
                Self::InspectConfiguration => "inspect-configuration",
                Self::PlanTarget => "plan-target",
            }
        }

        fn error_label(self) -> &'static str {
            match self {
                Self::InspectAttachment | Self::PlanAttachment => "attachment",
                Self::InspectConfiguration => "attachment host configuration",
                Self::PlanTarget => "target capabilities",
            }
        }
    }

    struct OwnedChild {
        child: Child,
        reaped: bool,
    }

    impl Drop for OwnedChild {
        fn drop(&mut self) {
            if !self.reaped {
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }
    }

    struct ProcessObservation {
        output: Output,
        exceeded_deadline: bool,
        cleanup_result: Option<String>,
    }

    fn run_bounded(
        directory: &TestDirectory,
        label: &str,
        arguments: &[OsString],
    ) -> ProcessObservation {
        let stdout_path = directory.path().join(format!("{label}.stdout"));
        let stderr_path = directory.path().join(format!("{label}.stderr"));
        let child = Command::new(env!("CARGO_BIN_EXE_attachment-native"))
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(File::create(&stdout_path).expect("owned stdout file"))
            .stderr(File::create(&stderr_path).expect("owned stderr file"))
            .spawn()
            .expect("actual CLI must start");
        let mut owned = OwnedChild {
            child,
            reaped: false,
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut exceeded_deadline = false;
        let mut cleanup_result = None;
        loop {
            if owned
                .child
                .try_wait()
                .expect("observe actual CLI")
                .is_some()
            {
                break;
            }
            if Instant::now() >= deadline {
                exceeded_deadline = true;
                cleanup_result = Some(match owned.child.kill() {
                    Ok(()) => "kill requested".to_string(),
                    Err(error) => format!("kill raced with exit or failed: {error}"),
                });
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let status = owned
            .child
            .wait()
            .expect("reap actual CLI before assertions");
        owned.reaped = true;
        ProcessObservation {
            output: Output {
                status,
                stdout: fs::read(stdout_path).expect("retained CLI stdout"),
                stderr: fs::read(stderr_path).expect("retained CLI stderr"),
            },
            exceeded_deadline,
            cleanup_result,
        }
    }

    fn target_fixture(directory: &TestDirectory) -> std::path::PathBuf {
        directory.write_json(
            "target.json",
            &json!({
                "target_id": "cli-fixture",
                "fingerprint": "cli-fixture:v1",
                "accepted_media_types": [],
                "accepted_media_families": [],
                "max_media_objects": 4,
                "max_media_bytes": 1048576,
                "max_text_bytes": 1048576,
                "supports_markdown": true,
                "supports_native_pdf": false,
                "supports_native_video": false
            }),
        )
    }

    fn arguments(
        role: FileRole,
        attachment: &std::path::Path,
        config: &std::path::Path,
        target: &std::path::Path,
    ) -> Vec<OsString> {
        match role {
            FileRole::InspectAttachment => vec!["inspect".into(), attachment.into()],
            FileRole::PlanAttachment | FileRole::PlanTarget => vec![
                "plan".into(),
                attachment.into(),
                "--target".into(),
                target.into(),
            ],
            FileRole::InspectConfiguration => vec![
                "inspect".into(),
                attachment.into(),
                "--config".into(),
                config.into(),
            ],
        }
    }

    fn selected_fifo_is_rejected(role: FileRole) {
        let directory = TestDirectory::new(role.label());
        let original = b"# Local note\n\nOwned public CLI fixture.\n";
        let attachment = directory.write("note.md", original);
        let config = directory.write_json(
            "config.json",
            &serde_json::to_value(AttachmentHostConfig::default())
                .expect("ordinary configuration must encode"),
        );
        let target = target_fixture(&directory);
        let config_bytes = fs::read(&config).expect("original configuration");
        let target_bytes = fs::read(&target).expect("original target");

        let ordinary = run(arguments(role, &attachment, &config, &target));
        assert_success(&ordinary);
        let value = output_json(&ordinary);
        assert_eq!(value["graph"]["schema"], "attachment_native.graph.v1");
        assert_eq!(
            value["graph"]["objects"][0]["detection"]["selected"],
            "markdown"
        );
        assert!(
            value["artifacts"]
                .as_array()
                .is_some_and(|items| !items.is_empty())
        );
        assert_eq!(value["receipt"]["network_used"], false);
        assert_eq!(value["receipt"]["process_used"], false);
        assert_eq!(value["receipt"]["model_invoked"], false);
        if matches!(role, FileRole::PlanAttachment | FileRole::PlanTarget) {
            assert_eq!(
                value["plan"]["schema"],
                "attachment_native.preparation_plan.v2"
            );
            assert_eq!(value["plan"]["target_id"], "cli-fixture");
            assert!(
                value["plan"]["parts"]
                    .as_array()
                    .is_some_and(|items| !items.is_empty())
            );
        }

        let fifo = directory.path().join("selected.fifo");
        nix::unistd::mkfifo(&fifo, Mode::S_IRUSR | Mode::S_IWUSR).expect("owned unpaired FIFO");
        let (selected_attachment, selected_config, selected_target) = match role {
            FileRole::InspectAttachment | FileRole::PlanAttachment => (&fifo, &config, &target),
            FileRole::InspectConfiguration => (&attachment, &fifo, &target),
            FileRole::PlanTarget => (&attachment, &config, &fifo),
        };
        let observation = run_bounded(
            &directory,
            role.label(),
            &arguments(role, selected_attachment, selected_config, selected_target),
        );

        // The child is already reaped; preservation checks precede the cause assertion.
        assert_eq!(
            fs::read(&attachment).expect("unchanged attachment"),
            original
        );
        assert_eq!(
            fs::read(&config).expect("unchanged configuration"),
            config_bytes
        );
        assert_eq!(fs::read(&target).expect("unchanged target"), target_bytes);
        use std::os::unix::fs::FileTypeExt as _;
        assert!(
            fs::symlink_metadata(&fifo)
                .expect("preserved FIFO")
                .file_type()
                .is_fifo()
        );
        eprintln!(
            "FIFO observation: {}",
            json!({
                "role": role.label(),
                "exceeded_deadline": observation.exceeded_deadline,
                "forced_cleanup": observation.cleanup_result,
                "reaped_status": observation.output.status.to_string(),
                "stdout_bytes": observation.output.stdout.len(),
                "stderr": stderr(&observation.output)
            })
        );
        assert!(
            !observation.exceeded_deadline,
            "{:?} did not return a regular-file refusal before the deadline; forced cleanup {:?}; reaped {}; stderr {}",
            role,
            observation.cleanup_result,
            observation.output.status,
            stderr(&observation.output)
        );
        assert_eq!(observation.output.status.code(), Some(2));
        assert!(observation.output.stdout.is_empty());
        let message = stderr(&observation.output);
        assert!(
            message.contains(role.error_label()),
            "wrong role diagnostic: {message}"
        );
        assert!(
            message.contains("is not a regular file"),
            "wrong kind diagnostic: {message}"
        );
    }

    #[test]
    fn inspect_attachment_fifo_is_rejected_after_ordinary_success() {
        selected_fifo_is_rejected(FileRole::InspectAttachment);
    }

    #[test]
    fn plan_attachment_fifo_is_rejected_after_ordinary_success() {
        selected_fifo_is_rejected(FileRole::PlanAttachment);
    }

    #[test]
    fn inspect_configuration_fifo_is_rejected_after_ordinary_success() {
        selected_fifo_is_rejected(FileRole::InspectConfiguration);
    }

    #[test]
    fn plan_target_fifo_is_rejected_after_ordinary_success() {
        selected_fifo_is_rejected(FileRole::PlanTarget);
    }

    #[test]
    fn relative_regular_symlinks_preserve_inspection_and_json_roles() {
        let directory = TestDirectory::new("relative-symlinks");
        let bytes = b"# Ordinary relative source\n";
        let attachment = directory.write("note.md", bytes);
        let config = directory.write_json(
            "config.json",
            &serde_json::to_value(AttachmentHostConfig::default()).expect("ordinary configuration"),
        );
        let target = target_fixture(&directory);
        let config_bytes = fs::read(config).expect("configuration bytes");
        let target_bytes = fs::read(target).expect("target bytes");
        for (original, link) in [
            ("note.md", "linked.md"),
            ("config.json", "linked-config.json"),
            ("target.json", "linked-target.json"),
        ] {
            std::os::unix::fs::symlink(original, directory.path().join(link))
                .expect("ordinary relative symlink");
        }
        for command in ["inspect", "plan"] {
            let mut process = Command::new(env!("CARGO_BIN_EXE_attachment-native"));
            process.current_dir(directory.path()).args([
                command,
                "linked.md",
                "--config",
                "linked-config.json",
            ]);
            if command == "plan" {
                process.args(["--target", "linked-target.json"]);
            }
            let output = process.output().expect("actual relative CLI");
            assert_success(&output);
            assert_eq!(
                output_json(&output)["graph"]["objects"][0]["detection"]["selected"],
                "markdown"
            );
        }
        assert_eq!(fs::read(attachment).expect("source bytes"), bytes);
        assert_eq!(
            fs::read(directory.path().join("config.json")).expect("configuration"),
            config_bytes
        );
        assert_eq!(
            fs::read(directory.path().join("target.json")).expect("target"),
            target_bytes
        );
    }
}
