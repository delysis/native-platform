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
