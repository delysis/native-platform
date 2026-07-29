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
        .env("MOM_LLAMA_DATA_DIR", root)
        .env("MOM_LLAMA_STORE_KEY_HEX", TEST_STORE_KEY)
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

fn mcp_fixture_server(root: &Path) -> Result<PathBuf> {
    let server = root.join("mcp-fixture");
    let body = r#"{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"echo","description":"Echo input","inputSchema":{"type":"object"}}],"content":[{"type":"text","text":"echo ok"}]}}"#;
    std::fs::write(
        &server,
        format!(
            "#!/bin/sh\nprintf 'Content-Length: {}\\r\\n\\r\\n{}'\n",
            body.len(),
            body
        ),
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&server, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(server)
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
fn attachment_and_mcp_are_exercisable_without_claiming_llama_inference() -> Result<()> {
    let root = data_dir("adapters")?;
    let image = root.join("photo.png");
    std::fs::write(&image, b"fixture image bytes")?;
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
    let mcp = mcp_fixture_server(&root)?;
    let mcp_path = mcp
        .to_str()
        .ok_or_else(|| anyhow!("invalid MCP fixture path"))?;
    json_output(&cli(
        &root,
        &[
            "mcp",
            "configure",
            "--name",
            "fixture",
            "--command",
            mcp_path,
            "--json",
        ],
    )?)?;
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
fn chat_and_consult_block_honestly_without_a_model() -> Result<()> {
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

    let consult = json_output(&cli(
        &root,
        &[
            "consult",
            "start",
            "--conversation",
            conversation_id,
            "--prompt",
            "Review this case",
            "--json",
        ],
    )?)?;
    assert_eq!(
        consult.get("status").and_then(Value::as_str),
        Some("blocked")
    );
    assert_eq!(
        consult
            .pointer("/receipt/real_engine_invoked")
            .and_then(Value::as_bool),
        Some(false)
    );
    Ok(())
}

#[test]
#[ignore = "requires MOM_LLAMA_MODEL_PATH pointing at a real local GGUF"]
fn real_native_cli_stream_uses_no_llama_executable() -> Result<()> {
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
        .env("MOM_LLAMA_DATA_DIR", &root)
        .env("MOM_LLAMA_STORE_KEY_HEX", TEST_STORE_KEY)
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
    assert!(
        lines
            .iter()
            .any(|line| { line.get("event").and_then(Value::as_str) == Some("delta") })
    );
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
