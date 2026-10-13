use anyhow::{Result, anyhow};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const TEST_STORE_KEY: &str = "1111111111111111111111111111111111111111111111111111111111111111";

fn data_dir(name: &str) -> Result<PathBuf> {
    let path = std::env::temp_dir().join(format!(
        "mom-llama-cli-{name}-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&path)?;
    Ok(path)
}

fn cli(root: &Path, args: &[&str]) -> Result<Output> {
    Ok(Command::new(env!("CARGO_BIN_EXE_mom-llama-cli"))
        .args(args)
        .env("LLAMA_NATIVE_KIT_DATA_DIR", root)
        .env("LLAMA_NATIVE_KIT_STORE_KEY_HEX", TEST_STORE_KEY)
        .env("HF_HUB_CACHE", root.join("model-cache"))
        .env_remove("MOM_LLAMA_MODEL_PATH")
        .env_remove("MOM_LLAMA_ENGINE_PATH")
        .output()?)
}

fn json_output(output: &Output) -> Result<Value> {
    if !output.status.success() {
        return Err(anyhow!(
            "command failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

#[cfg(unix)]
fn mcp_fixture_server(root: &Path) -> Result<(PathBuf, Vec<String>)> {
    let body = r#"{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"echo","description":"Echo input","inputSchema":{"type":"object"}}],"content":[{"type":"text","text":"echo ok"}]}}"#;
    let initialize = r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}}}"#;
    use std::os::unix::fs::PermissionsExt;
    let server = root.join("mcp-fixture");
    std::fs::write(
        &server,
        format!(
            "#!/bin/sh\nIFS= read -r initialize\nprintf '%s\\n' '{}'\nIFS= read -r initialized\nIFS= read -r request\nprintf '%s\\n' '{}'\n",
            initialize, body
        ),
    )?;
    std::fs::set_permissions(&server, std::fs::Permissions::from_mode(0o700))?;
    Ok((server, Vec::new()))
}

#[test]
fn engine_check_returns_typed_missing_model_blocker_without_executable_config() -> Result<()> {
    let root = data_dir("missing-model")?;
    let value = json_output(&cli(&root, &["engine", "check", "--json"])?)?;
    assert_eq!(value.get("status").and_then(Value::as_str), Some("blocked"));
    assert_eq!(
        value.pointer("/blocker/code").and_then(Value::as_str),
        Some("model_path_missing")
    );
    assert_eq!(
        value
            .pointer("/receipt/real_engine_invoked")
            .and_then(Value::as_bool),
        Some(false)
    );
    assert_eq!(
        value
            .pointer("/receipt/fake_fixture")
            .and_then(Value::as_bool),
        Some(false)
    );
    assert!(!root.join("settings.json").exists());
    Ok(())
}

#[test]
fn settings_skills_and_search_persist_in_encrypted_sqlite() -> Result<()> {
    let root = data_dir("persistence")?;
    let settings = json_output(&cli(
        &root,
        &[
            "settings",
            "update",
            "--device",
            "cpu",
            "--context-tokens",
            "2048",
            "--max-parallel-sequences",
            "4",
            "--set",
            "sendOnEnter=false",
            "--set",
            "systemMessage=Be kind.",
            "--json",
        ],
    )?)?;
    assert_eq!(
        settings
            .pointer("/result/native_device")
            .and_then(Value::as_str),
        Some("cpu")
    );
    assert_eq!(
        settings.pointer("/result/upstream_settings/sendOnEnter"),
        Some(&Value::Bool(false))
    );

    let created = json_output(&cli(
        &root,
        &[
            "skill",
            "create",
            "--name",
            "Friendly explainer",
            "--prompt-template",
            "Explain this simply:",
            "--json",
        ],
    )?)?;
    let skill_id = created
        .pointer("/result/id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing skill id"))?;
    json_output(&cli(
        &root,
        &[
            "skill",
            "edit",
            "--skill",
            skill_id,
            "--name",
            "Friendly clinical explainer",
            "--prompt-template",
            "Explain carefully in plain language:",
            "--json",
        ],
    )?)?;
    let skills = json_output(&cli(&root, &["skill", "list", "--json"])?)?;
    assert_eq!(
        skills.pointer("/result/0/name").and_then(Value::as_str),
        Some("Friendly clinical explainer")
    );

    let conversation = json_output(&cli(
        &root,
        &[
            "conversation",
            "new",
            "--title",
            "Garden planning",
            "--json",
        ],
    )?)?;
    let conversation_id = conversation
        .pointer("/result/id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing conversation id"))?;
    let search = json_output(&cli(
        &root,
        &["conversation", "search", "--query", "garden", "--json"],
    )?)?;
    assert_eq!(
        search
            .pointer("/result/0/conversation_id")
            .and_then(Value::as_str),
        Some(conversation_id)
    );

    let database = std::fs::read(root.join("runtime.sqlite3"))?;
    let raw = String::from_utf8_lossy(&database);
    assert!(!raw.contains("Friendly clinical explainer"));
    assert!(!raw.contains("Garden planning"));
    Ok(())
}

#[test]
fn cache_policy_cli_uses_plain_names() -> Result<()> {
    let root = data_dir("cache-policy-cli")?;
    for (argument, expected, preencode) in [
        ("automatic", "kv_cache_candidate", true),
        ("prefixes-only", "prompt_prefix", false),
        ("off", "none", false),
    ] {
        let value = json_output(&cli(
            &root,
            &[
                "settings",
                "update",
                "--kv-cache-policy",
                argument,
                "--json",
            ],
        )?)?;
        assert_eq!(
            value
                .pointer("/result/kv_cache_policy")
                .and_then(Value::as_str),
            Some(expected),
            "unexpected policy for {argument}"
        );
        assert_eq!(
            value.pointer("/result/upstream_settings/preEncodeConversation"),
            Some(&Value::Bool(preencode)),
            "unexpected checkpoint setting for {argument}"
        );
    }
    Ok(())
}

#[test]
fn cli_imports_long_paste_directly_into_encrypted_storage() -> Result<()> {
    let root = data_dir("paste-attachment")?;
    let marker = "private-cli-paste-4271 ".repeat(120);
    let value = json_output(&cli(
        &root,
        &[
            "attachment",
            "import-paste",
            "--conversation",
            "default",
            "--text",
            &marker,
            "--json",
        ],
    )?)?;
    assert_eq!(
        value.get("readiness").and_then(Value::as_str),
        Some("contracted")
    );
    assert_eq!(
        value
            .pointer("/result/attachment/source_path")
            .and_then(Value::as_str),
        Some("pasted-text")
    );
    let database = std::fs::read(root.join("runtime.sqlite3"))?;
    assert!(
        !database
            .windows(marker.len())
            .any(|window| window == marker.as_bytes())
    );
    Ok(())
}

#[test]
fn cli_attachment_preview_returns_metadata_without_decrypted_payload() -> Result<()> {
    let root = data_dir("attachment-preview")?;
    let image = root.join("preview.png");
    let private_payload = b"private-preview-payload-7331";
    std::fs::write(&image, private_payload)?;
    let imported = json_output(&cli(
        &root,
        &[
            "attachment",
            "import",
            "--conversation",
            "default",
            "--path",
            image.to_str().ok_or_else(|| anyhow!("invalid test path"))?,
            "--json",
        ],
    )?)?;
    let attachment_id = imported
        .pointer("/result/attachment/id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing attachment id"))?;
    let preview = json_output(&cli(
        &root,
        &[
            "attachment",
            "preview",
            "--attachment",
            attachment_id,
            "--json",
        ],
    )?)?;
    assert_eq!(
        preview
            .pointer("/result/attachment_id")
            .and_then(Value::as_str),
        Some(attachment_id)
    );
    assert_eq!(
        preview.pointer("/result/schema").and_then(Value::as_str),
        Some("mom_llama.attachment_preview_catalog.v1")
    );
    assert!(preview.pointer("/result/bytes").is_none());
    assert!(preview.pointer("/result/source_path").is_none());
    assert!(preview.pointer("/result/stored_path").is_none());
    let database = std::fs::read(root.join("runtime.sqlite3"))?;
    assert!(
        !database
            .windows(private_payload.len())
            .any(|window| window == private_payload)
    );
    Ok(())
}

#[test]
fn path_selection_is_cli_exercisable_and_typed() -> Result<()> {
    let root = data_dir("path-selection")?;
    let model = root.join("tiny.gguf");
    std::fs::write(&model, b"GGUF")?;
    let selected = json_output(&cli(
        &root,
        &[
            "path",
            "select",
            "--kind",
            "model",
            "--path",
            model.to_str().ok_or_else(|| anyhow!("invalid test path"))?,
            "--json",
        ],
    )?)?;
    assert_eq!(selected["command"], "mom_llama.path_select");
    assert_eq!(selected["status"], "contracted");
    assert_eq!(selected["readiness"], "contracted");
    assert_eq!(selected["result"]["kind"], "model");
    assert_eq!(
        selected["result"]["path"],
        model.canonicalize()?.display().to_string()
    );
    Ok(())
}

#[test]
fn attachment_and_mcp_are_exercisable_without_claiming_llama_inference() -> Result<()> {
    let root = data_dir("adapters")?;
    let image = root.join("photo.png");
    std::fs::write(&image, b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x02\0\0\0\x03")?;
    let conversation = json_output(&cli(
        &root,
        &["conversation", "new", "--title", "Pictures", "--json"],
    )?)?;
    let conversation_id = conversation
        .pointer("/result/id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing conversation id"))?;
    let image_path = image
        .to_str()
        .ok_or_else(|| anyhow!("invalid fixture path"))?;
    let attachment = json_output(&cli(
        &root,
        &[
            "attachment",
            "import",
            "--conversation",
            conversation_id,
            "--path",
            image_path,
            "--json",
        ],
    )?)?;
    assert_eq!(
        attachment
            .pointer("/result/attachment/kind")
            .and_then(Value::as_str),
        Some("image")
    );
    assert!(
        attachment
            .pointer("/result/attachment/stored_path")
            .and_then(Value::as_str)
            .is_some_and(|path| path.starts_with("encrypted://"))
    );

    #[cfg(unix)]
    {
        json_output(&cli(
            &root,
            &[
                "settings",
                "update",
                "--set",
                "mcpNativeEnabled=true",
                "--json",
            ],
        )?)?;
        let (mcp, mcp_args) = mcp_fixture_server(&root)?;
        let mcp_path = mcp
            .to_str()
            .ok_or_else(|| anyhow!("invalid MCP fixture path"))?;
        let mut configure_args = vec![
            "mcp".to_string(),
            "configure".to_string(),
            "--name".to_string(),
            "fixture".to_string(),
            "--command".to_string(),
            mcp_path.to_string(),
        ];
        for argument in mcp_args {
            configure_args.push(format!("--arg={argument}"));
        }
        configure_args.push("--json".to_string());
        let configure_refs = configure_args
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        json_output(&cli(&root, &configure_refs)?)?;
        let tools = json_output(&cli(
            &root,
            &["mcp", "list-tools", "--server", "fixture", "--json"],
        )?)?;
        assert_eq!(
            tools.pointer("/result/0/name").and_then(Value::as_str),
            Some("echo")
        );
        assert_eq!(
            tools
                .pointer("/receipt/real_engine_invoked")
                .and_then(Value::as_bool),
            Some(false)
        );
    }
    Ok(())
}

#[test]
fn chat_blocks_honestly_without_a_model() -> Result<()> {
    let root = data_dir("generation-blockers")?;
    let conversation = json_output(&cli(
        &root,
        &["conversation", "new", "--title", "Blocked", "--json"],
    )?)?;
    let conversation_id = conversation
        .pointer("/result/id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing conversation id"))?;

    let chat = json_output(&cli(
        &root,
        &[
            "chat",
            "send",
            "--conversation",
            conversation_id,
            "--message",
            "hello",
            "--json",
        ],
    )?)?;
    assert_eq!(chat.get("status").and_then(Value::as_str), Some("blocked"));
    assert_eq!(
        chat.pointer("/blocker/code").and_then(Value::as_str),
        Some("model_path_missing")
    );
    Ok(())
}

#[test]
fn tool_loop_status_and_cancel_are_typed_without_an_active_run() -> Result<()> {
    let root = data_dir("tool-loop-supervision")?;
    let status = json_output(&cli(
        &root,
        &[
            "tool-loop",
            "status",
            "--conversation",
            "not-running",
            "--json",
        ],
    )?)?;
    assert!(
        status
            .pointer("/result")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
    );
    assert_eq!(
        status
            .pointer("/receipt/real_engine_invoked")
            .and_then(Value::as_bool),
        Some(false)
    );

    let cancelled = json_output(&cli(
        &root,
        &[
            "tool-loop",
            "cancel",
            "--conversation",
            "not-running",
            "--json",
        ],
    )?)?;
    assert_eq!(
        cancelled.get("status").and_then(Value::as_str),
        Some("blocked")
    );
    assert_eq!(
        cancelled.pointer("/blocker/code").and_then(Value::as_str),
        Some("no_active_tool_loop")
    );
    assert_eq!(
        cancelled
            .pointer("/receipt/real_engine_invoked")
            .and_then(Value::as_bool),
        Some(false)
    );
    Ok(())
}

#[test]
fn skip_reasoning_is_typed_without_an_active_generation() -> Result<()> {
    let root = data_dir("skip-reasoning")?;
    let value = json_output(&cli(
        &root,
        &[
            "chat",
            "skip-reasoning",
            "--conversation",
            "not-running",
            "--json",
        ],
    )?)?;
    assert_eq!(value.get("status").and_then(Value::as_str), Some("blocked"));
    assert_eq!(
        value.pointer("/blocker/code").and_then(Value::as_str),
        Some("no_active_reasoning_request")
    );
    assert_eq!(
        value
            .pointer("/receipt/real_engine_invoked")
            .and_then(Value::as_bool),
        Some(false)
    );
    Ok(())
}

#[test]
#[ignore = "requires MOM_LLAMA_MODEL_PATH pointing at a real local GGUF"]
fn real_native_cli_stream_preserves_inference_evidence_without_executable() -> Result<()> {
    let Some(model) = std::env::var_os("MOM_LLAMA_MODEL_PATH").map(PathBuf::from) else {
        return Ok(());
    };
    if !model.is_file() {
        return Ok(());
    }
    let root = data_dir("real-native")?;
    let model_path = model
        .to_str()
        .ok_or_else(|| anyhow!("invalid model path"))?;
    let output = Command::new(env!("CARGO_BIN_EXE_mom-llama-cli"))
        .args([
            "chat",
            "send",
            "--conversation",
            "default",
            "--message",
            "Reply with the word native.",
            "--stream-jsonl",
            "--json",
        ])
        .env("LLAMA_NATIVE_KIT_DATA_DIR", &root)
        .env("LLAMA_NATIVE_KIT_STORE_KEY_HEX", TEST_STORE_KEY)
        .env("MOM_LLAMA_MODEL_PATH", model_path)
        .env_remove("MOM_LLAMA_ENGINE_PATH")
        .output()?;
    if !output.status.success() {
        return Err(anyhow!(
            "real native stream failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let lines = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert!(lines.iter().any(|line| {
        matches!(
            line.get("event").and_then(Value::as_str),
            Some("delta" | "reasoning_delta")
        ) && line.get("real_engine_invoked").and_then(Value::as_bool) == Some(true)
    }));
    let result = lines.last().ok_or_else(|| anyhow!("missing result"))?;
    assert_eq!(
        result
            .pointer("/receipt/real_engine_invoked")
            .and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        result
            .pointer("/receipt/fake_fixture")
            .and_then(Value::as_bool),
        Some(false)
    );
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn attachment_import_refuses_owned_fifo_without_mutating_author_rows() -> Result<()> {
    use nix::sys::stat::Mode;
    use rusqlite::{Connection, OpenFlags};
    use std::fs::{self, File};
    use std::os::unix::fs::{FileTypeExt, symlink};
    use std::process::{Child, Stdio};
    use std::time::{Duration, Instant};

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

    let root = data_dir("attachment-fifo")?;
    let source = b"Owned ordinary import.\n";
    fs::write(root.join("ordinary.txt"), source)?;
    symlink("ordinary.txt", root.join("ordinary-link.txt"))?;
    fs::create_dir(root.join("directory"))?;
    nix::unistd::mkfifo(&root.join("selected.fifo"), Mode::S_IRUSR | Mode::S_IWUSR)?;
    let created = json_output(&cli(
        &root,
        &["conversation", "new", "--title", "Owned import", "--json"],
    )?)?;
    let conversation = created
        .pointer("/result/id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing conversation id"))?;

    type EncryptedDocument = (String, Vec<u8>, Vec<u8>, i64);
    let encrypted_documents = || -> Result<Vec<EncryptedDocument>> {
        let connection = Connection::open_with_flags(
            root.join("runtime.sqlite3"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let mut statement = connection.prepare(
            "SELECT namespace, nonce, ciphertext, updated_at
             FROM encrypted_documents ORDER BY namespace",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    };

    for (path, blocker) in [
        ("ordinary.txt", None),
        ("ordinary-link.txt", None),
        ("missing.txt", Some("attachment_not_found")),
        ("directory", Some("attachment_not_file")),
        ("selected.fifo", Some("attachment_not_file")),
    ] {
        let before_documents = encrypted_documents()?;
        let stdout_path = root.join(format!("{path}.stdout"));
        let stderr_path = root.join(format!("{path}.stderr"));
        let child = Command::new(env!("CARGO_BIN_EXE_mom-llama-cli"))
            .current_dir(&root)
            .args([
                "attachment",
                "import",
                "--conversation",
                conversation,
                "--path",
                path,
                "--json",
            ])
            .env("LLAMA_NATIVE_KIT_DATA_DIR", &root)
            .env("LLAMA_NATIVE_KIT_STORE_KEY_HEX", TEST_STORE_KEY)
            .env("HF_HUB_CACHE", root.join("model-cache"))
            .env_remove("MOM_LLAMA_MODEL_PATH")
            .env_remove("MOM_LLAMA_ENGINE_PATH")
            .stdin(Stdio::null())
            .stdout(File::create(&stdout_path)?)
            .stderr(File::create(&stderr_path)?)
            .spawn()?;
        let mut owned = OwnedChild {
            child,
            reaped: false,
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut exceeded_deadline = false;
        let mut cleanup_result = None;
        loop {
            if owned.child.try_wait()?.is_some() {
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
        let status = owned.child.wait()?;
        owned.reaped = true;
        let output = Output {
            status,
            stdout: fs::read(stdout_path)?,
            stderr: fs::read(stderr_path)?,
        };
        let after_documents = encrypted_documents()?;
        eprintln!(
            "owned import path={path} exceeded_deadline={exceeded_deadline} cleanup={cleanup_result:?} status={}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(fs::read(root.join("ordinary.txt"))?, source);
        assert_eq!(
            fs::read_link(root.join("ordinary-link.txt"))?,
            Path::new("ordinary.txt")
        );
        assert!(
            fs::symlink_metadata(root.join("selected.fifo"))?
                .file_type()
                .is_fifo()
        );
        if blocker.is_some() {
            for namespace in ["conversations.v2", "drafts.v2", "attachments.v3"] {
                assert!(before_documents.iter().any(|row| row.0 == namespace));
            }
            assert_eq!(
                after_documents, before_documents,
                "author rows changed for {path}"
            );
        }
        assert!(
            !exceeded_deadline,
            "actual CLI import did not refuse or finish before the deadline for {path}"
        );
        let value = json_output(&output)?;
        assert_eq!(
            value
                .pointer("/receipt/real_engine_invoked")
                .and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(
            value
                .pointer("/receipt/fake_fixture")
                .and_then(Value::as_bool),
            Some(false)
        );
        if let Some(blocker) = blocker {
            assert_eq!(value.get("status").and_then(Value::as_str), Some("blocked"));
            assert_eq!(
                value.get("readiness").and_then(Value::as_str),
                Some("stub_blocked")
            );
            assert_eq!(
                value.pointer("/blocker/code").and_then(Value::as_str),
                Some(blocker)
            );
        } else {
            assert_eq!(
                value.get("status").and_then(Value::as_str),
                Some("contracted")
            );
            assert_eq!(
                value
                    .pointer("/result/attachment/source_path")
                    .and_then(Value::as_str),
                Some(path)
            );
            assert_eq!(
                value
                    .pointer("/result/attachment/conversation_id")
                    .and_then(Value::as_str),
                Some(conversation)
            );
            assert_eq!(
                value
                    .pointer("/result/attachment/state")
                    .and_then(Value::as_str),
                Some("staged")
            );
            assert_eq!(
                value
                    .pointer("/result/attachment/bytes")
                    .and_then(Value::as_u64),
                Some(source.len() as u64)
            );
            assert_eq!(
                value
                    .pointer("/result/attachment/sha256")
                    .and_then(Value::as_str),
                Some("de732d807a7adaecdcce81deb01457e010718969dafb1c0a8482e77ba963ce6e")
            );
            let attachment_id = value
                .pointer("/result/attachment/id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("missing attachment id"))?;
            uuid::Uuid::parse_str(attachment_id)?;
            assert!(
                after_documents
                    .iter()
                    .any(|row| row.0 == format!("attachment.manifest.{attachment_id}"))
            );
        }
        let receipt_id = value
            .pointer("/receipt/task_id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("missing command receipt id"))?;
        let connection = Connection::open_with_flags(
            root.join("runtime.sqlite3"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let receipt_command: String = connection.query_row(
            "SELECT command_id FROM receipts WHERE receipt_id = ?1",
            [receipt_id],
            |row| row.get(0),
        )?;
        assert_eq!(receipt_command, "mom_llama.attachment_import");
    }
    Ok(())
}
