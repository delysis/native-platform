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

fn mcp_fixture_server(root: &Path) -> Result<(PathBuf, Vec<String>)> {
    #[cfg(windows)]
    let _ = root;
    let body = r#"{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"echo","description":"Echo input","inputSchema":{"type":"object"}}],"content":[{"type":"text","text":"echo ok"}]}}"#;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let server = root.join("mcp-fixture");
        std::fs::write(
            &server,
            format!(
                "#!/bin/sh\ncat >/dev/null\nprintf 'Content-Length: {}\\r\\n\\r\\n{}'\n",
                body.len(),
                body
            ),
        )?;
        std::fs::set_permissions(&server, std::fs::Permissions::from_mode(0o700))?;
        Ok((server, Vec::new()))
    }
    #[cfg(windows)]
    {
        let system_root = std::env::var_os("SystemRoot")
            .ok_or_else(|| anyhow!("SystemRoot is not configured"))?;
        let powershell = PathBuf::from(system_root)
            .join("System32")
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe");
        let escaped_body = body
            .replace('`', "``")
            .replace('$', "`$")
            .replace('"', "`\"");
        let script = format!(
            "[Console]::In.ReadToEnd() | Out-Null; [Console]::Out.Write(\"Content-Length: {}`r`n`r`n{}\")",
            body.len(),
            escaped_body
        );
        Ok((
            powershell,
            vec![
                "-NoProfile".to_string(),
                "-NonInteractive".to_string(),
                "-Command".to_string(),
                script,
            ],
        ))
    }
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
fn cache_policy_cli_uses_plain_names_and_preserves_legacy_aliases() -> Result<()> {
    let root = data_dir("cache-policy-cli")?;
    for (argument, expected, preencode) in [
        ("automatic", "kv_cache_candidate", true),
        ("prefixes-only", "prompt_prefix", false),
        ("off", "none", false),
        ("prompt-prefix", "prompt_prefix", false),
        ("kv-cache-candidate", "kv_cache_candidate", true),
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
            .pointer("/result/attachment/id")
            .and_then(Value::as_str),
        Some(attachment_id)
    );
    assert!(preview.pointer("/result/bytes").is_none());
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
fn legacy_consult_cli_is_hidden_but_remains_available_for_recovery() -> Result<()> {
    let root = data_dir("legacy-consult-recovery")?;
    let help = cli(&root, &["--help"])?;
    assert!(help.status.success());
    let help = String::from_utf8(help.stdout)?;
    assert!(
        !help
            .lines()
            .any(|line| line.trim_start().starts_with("consult")),
        "legacy Consult commands must not be advertised in the product CLI"
    );

    let recovered = json_output(&cli(&root, &["consult", "panel-list", "--json"])?)?;
    assert_eq!(
        recovered.get("command").and_then(Value::as_str),
        Some("mom_llama.consult_panel_list")
    );
    assert_eq!(
        recovered.get("readiness").and_then(Value::as_str),
        Some("contracted")
    );
    Ok(())
}

#[test]
fn legacy_consult_cli_rejects_every_mutating_subcommand() -> Result<()> {
    let root = data_dir("legacy-consult-read-only")?;
    let help = cli(&root, &["consult", "--help"])?;
    assert!(help.status.success());
    let help = String::from_utf8(help.stdout)?;
    for retired in ["panel-create", "start", "cancel", "synthesize"] {
        assert!(
            !help.lines().any(|line| line.contains(retired)),
            "retired mutating subcommand `{retired}` must not be parseable"
        );
    }

    for args in [
        &["consult", "panel-create"][..],
        &["consult", "start"][..],
        &["consult", "cancel"][..],
        &["consult", "synthesize"][..],
    ] {
        let rejected = cli(&root, args)?;
        assert!(
            !rejected.status.success(),
            "retired mutating command `{}` unexpectedly succeeded",
            args.join(" ")
        );
        assert!(
            String::from_utf8_lossy(&rejected.stderr).contains("unrecognized subcommand"),
            "retired command `{}` did not fail during parsing",
            args.join(" ")
        );
    }
    assert!(
        !root.join("runtime.sqlite3").exists(),
        "rejected legacy writes must not initialize product storage"
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
    Ok(())
}

#[test]
fn deprecated_server_alias_is_hidden_and_never_opens_a_server() -> Result<()> {
    let root = data_dir("server-alias")?;
    let help = cli(&root, &["--help"])?;
    assert!(help.status.success());
    let help_text = String::from_utf8_lossy(&help.stdout);
    assert!(!help_text.contains("server"));

    let status = json_output(&cli(&root, &["server", "status", "--json"])?)?;
    assert_eq!(
        status.pointer("/result/transport").and_then(Value::as_str),
        Some("in_process")
    );
    assert_eq!(
        status.pointer("/result/running").and_then(Value::as_bool),
        Some(false)
    );
    assert_eq!(
        status
            .pointer("/result/resident_models")
            .and_then(Value::as_u64),
        Some(0)
    );
    let result = status
        .get("result")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("missing native status"))?;
    for forbidden in ["host", "port", "pid", "server_path"] {
        assert!(!result.contains_key(forbidden));
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
