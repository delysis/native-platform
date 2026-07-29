use crate::config::{resolve_settings, upstream_setting_i64};
use crate::receipts::{Blocker, CommandResult};
use crate::store::RuntimeStore;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const MCP_SERVERS_FILE: &str = "mcp-servers.json";
const MCP_SERVERS_NAMESPACE: &str = "mcp-servers.v2";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpServerConfig {
    pub name: String,
    pub command: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct McpServerDb {
    #[serde(default)]
    pub servers: Vec<McpServerConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpStatus {
    pub enabled: bool,
    pub server_count: usize,
    pub servers: Vec<McpServerConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpTool {
    pub name: String,
    pub description: Option<String>,
    pub input_schema: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpCallToolOutput {
    pub server: String,
    pub tool: String,
    pub content: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpResource {
    pub uri: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub mime_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpResourceContent {
    pub uri: Option<String>,
    pub mime_type: Option<String>,
    pub text: Option<String>,
    pub blob: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpReadResourceOutput {
    pub server: String,
    pub uri: String,
    pub contents: Vec<McpResourceContent>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpPromptArgument {
    pub name: String,
    pub description: Option<String>,
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpPrompt {
    pub name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub arguments: Vec<McpPromptArgument>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpGetPromptOutput {
    pub server: String,
    pub prompt: String,
    pub description: Option<String>,
    pub messages: Value,
}

pub fn mcp_status() -> Result<CommandResult<McpStatus>> {
    let db = load_mcp_db()?;
    let enabled = mcp_enabled()?;
    let result = McpStatus {
        enabled,
        server_count: db.servers.len(),
        servers: db.servers,
    };
    if !enabled {
        return Ok(CommandResult::blocked(
            "mom_llama.mcp_status",
            "stub_blocked",
            Blocker::new(
                "mcp_native_disabled",
                "Native MCP execution is disabled until explicitly enabled in settings.",
                vec![
                    "Run `mom-llama settings update --set mcpNativeEnabled=true --json`."
                        .to_string(),
                ],
            ),
        )
        .with_result(result));
    }
    Ok(CommandResult::passed(
        "mom_llama.mcp_status",
        "contracted",
        result,
        Vec::new(),
        Vec::new(),
        false,
        false,
    ))
}

pub fn mcp_configure(
    name: String,
    command: PathBuf,
    args: Vec<String>,
    enabled: bool,
) -> Result<CommandResult<McpServerConfig>> {
    if let Some(blocker) = validate_mcp_command(&command) {
        return Ok(CommandResult::blocked(
            "mom_llama.mcp_configure",
            "blocked_invalid_mcp_server",
            blocker,
        ));
    }
    let mut db = load_mcp_db()?;
    let config = McpServerConfig {
        name: name.trim().to_string(),
        command,
        args,
        enabled,
    };
    if config.name.is_empty() {
        return Ok(CommandResult::blocked(
            "mom_llama.mcp_configure",
            "stub_blocked",
            Blocker::new(
                "mcp_server_name_empty",
                "MCP server name is empty.",
                vec!["Choose a stable server name.".to_string()],
            ),
        ));
    }
    if let Some(existing) = db
        .servers
        .iter_mut()
        .find(|server| server.name == config.name)
    {
        *existing = config.clone();
    } else {
        db.servers.push(config.clone());
    }
    let path = save_mcp_db(&db)?;
    Ok(CommandResult::passed(
        "mom_llama.mcp_configure",
        "contracted",
        config,
        vec![path.display().to_string()],
        Vec::new(),
        false,
        false,
    ))
}

pub fn mcp_list_servers() -> Result<CommandResult<Vec<McpServerConfig>>> {
    let db = load_mcp_db()?;
    Ok(CommandResult::passed(
        "mom_llama.mcp_list_servers",
        "contracted",
        db.servers,
        Vec::new(),
        Vec::new(),
        false,
        false,
    ))
}

pub fn mcp_list_tools(server_name: &str) -> Result<CommandResult<Vec<McpTool>>> {
    let server = match enabled_server(server_name)? {
        Ok(server) => server,
        Err((readiness, blocker)) => {
            return Ok(CommandResult::blocked(
                "mom_llama.mcp_list_tools",
                &readiness,
                blocker,
            ));
        }
    };
    let response = execute_mcp_request(&server, "tools/list", json!({}))?;
    let tools = response
        .pointer("/result/tools")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|tool| {
            let name = tool.get("name").and_then(Value::as_str)?.to_string();
            Some(McpTool {
                name,
                description: tool
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                input_schema: tool
                    .get("inputSchema")
                    .cloned()
                    .unwrap_or_else(|| json!({"type":"object"})),
            })
        })
        .collect::<Vec<_>>();
    Ok(CommandResult::passed(
        "mom_llama.mcp_list_tools",
        "host_integrated",
        tools,
        Vec::new(),
        Vec::new(),
        false,
        false,
    ))
}

pub fn mcp_call_tool(
    server_name: &str,
    tool_name: &str,
    arguments: Value,
) -> Result<CommandResult<McpCallToolOutput>> {
    let server = match enabled_server(server_name)? {
        Ok(server) => server,
        Err((readiness, blocker)) => {
            return Ok(CommandResult::blocked(
                "mom_llama.mcp_call_tool",
                &readiness,
                blocker,
            ));
        }
    };
    if tool_name.trim().is_empty() {
        return Ok(CommandResult::blocked(
            "mom_llama.mcp_call_tool",
            "stub_blocked",
            Blocker::new(
                "mcp_tool_name_empty",
                "MCP tool name is empty.",
                vec!["Choose a tool returned by `mcp list-tools`.".to_string()],
            ),
        ));
    }
    let response = execute_mcp_request(
        &server,
        "tools/call",
        json!({
            "name": tool_name,
            "arguments": arguments,
        }),
    )?;
    Ok(CommandResult::passed(
        "mom_llama.mcp_call_tool",
        "host_integrated",
        McpCallToolOutput {
            server: server.name,
            tool: tool_name.to_string(),
            content: response.get("result").cloned().unwrap_or(response),
        },
        Vec::new(),
        Vec::new(),
        false,
        false,
    ))
}

pub fn mcp_list_resources(server_name: &str) -> Result<CommandResult<Vec<McpResource>>> {
    let server = match enabled_server(server_name)? {
        Ok(server) => server,
        Err((readiness, blocker)) => {
            return Ok(CommandResult::blocked(
                "mom_llama.mcp_list_resources",
                &readiness,
                blocker,
            ));
        }
    };
    let response = execute_mcp_request(&server, "resources/list", json!({}))?;
    let resources = response
        .pointer("/result/resources")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|resource| {
            let uri = resource.get("uri").and_then(Value::as_str)?.to_string();
            Some(McpResource {
                uri,
                name: resource
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                description: resource
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                mime_type: resource
                    .get("mimeType")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
        })
        .collect::<Vec<_>>();
    Ok(CommandResult::passed(
        "mom_llama.mcp_list_resources",
        "host_integrated",
        resources,
        Vec::new(),
        Vec::new(),
        false,
        false,
    ))
}

pub fn mcp_read_resource(
    server_name: &str,
    uri: &str,
) -> Result<CommandResult<McpReadResourceOutput>> {
    let server = match enabled_server(server_name)? {
        Ok(server) => server,
        Err((readiness, blocker)) => {
            return Ok(CommandResult::blocked(
                "mom_llama.mcp_read_resource",
                &readiness,
                blocker,
            ));
        }
    };
    if uri.trim().is_empty() {
        return Ok(CommandResult::blocked(
            "mom_llama.mcp_read_resource",
            "stub_blocked",
            Blocker::new(
                "mcp_resource_uri_empty",
                "MCP resource URI is empty.",
                vec!["Choose a resource returned by `mcp list-resources`.".to_string()],
            ),
        ));
    }
    let response = execute_mcp_request(&server, "resources/read", json!({ "uri": uri }))?;
    let contents = response
        .pointer("/result/contents")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|content| McpResourceContent {
            uri: content
                .get("uri")
                .and_then(Value::as_str)
                .map(str::to_string),
            mime_type: content
                .get("mimeType")
                .and_then(Value::as_str)
                .map(str::to_string),
            text: content
                .get("text")
                .and_then(Value::as_str)
                .map(str::to_string),
            blob: content
                .get("blob")
                .and_then(Value::as_str)
                .map(str::to_string),
        })
        .collect::<Vec<_>>();
    Ok(CommandResult::passed(
        "mom_llama.mcp_read_resource",
        "host_integrated",
        McpReadResourceOutput {
            server: server.name,
            uri: uri.to_string(),
            contents,
        },
        Vec::new(),
        Vec::new(),
        false,
        false,
    ))
}

pub fn mcp_list_prompts(server_name: &str) -> Result<CommandResult<Vec<McpPrompt>>> {
    let server = match enabled_server(server_name)? {
        Ok(server) => server,
        Err((readiness, blocker)) => {
            return Ok(CommandResult::blocked(
                "mom_llama.mcp_list_prompts",
                &readiness,
                blocker,
            ));
        }
    };
    let response = execute_mcp_request(&server, "prompts/list", json!({}))?;
    let prompts = response
        .pointer("/result/prompts")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|prompt| {
            let name = prompt.get("name").and_then(Value::as_str)?.to_string();
            let arguments = prompt
                .get("arguments")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|argument| {
                    let name = argument.get("name").and_then(Value::as_str)?.to_string();
                    Some(McpPromptArgument {
                        name,
                        description: argument
                            .get("description")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        required: argument
                            .get("required")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    })
                })
                .collect::<Vec<_>>();
            Some(McpPrompt {
                name,
                description: prompt
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                arguments,
            })
        })
        .collect::<Vec<_>>();
    Ok(CommandResult::passed(
        "mom_llama.mcp_list_prompts",
        "host_integrated",
        prompts,
        Vec::new(),
        Vec::new(),
        false,
        false,
    ))
}

pub fn mcp_get_prompt(
    server_name: &str,
    prompt_name: &str,
    arguments: Value,
) -> Result<CommandResult<McpGetPromptOutput>> {
    let server = match enabled_server(server_name)? {
        Ok(server) => server,
        Err((readiness, blocker)) => {
            return Ok(CommandResult::blocked(
                "mom_llama.mcp_get_prompt",
                &readiness,
                blocker,
            ));
        }
    };
    if prompt_name.trim().is_empty() {
        return Ok(CommandResult::blocked(
            "mom_llama.mcp_get_prompt",
            "stub_blocked",
            Blocker::new(
                "mcp_prompt_name_empty",
                "MCP prompt name is empty.",
                vec!["Choose a prompt returned by `mcp list-prompts`.".to_string()],
            ),
        ));
    }
    let response = execute_mcp_request(
        &server,
        "prompts/get",
        json!({
            "name": prompt_name,
            "arguments": arguments,
        }),
    )?;
    let result = response.get("result").cloned().unwrap_or_else(|| json!({}));
    Ok(CommandResult::passed(
        "mom_llama.mcp_get_prompt",
        "host_integrated",
        McpGetPromptOutput {
            server: server.name,
            prompt: prompt_name.to_string(),
            description: result
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_string),
            messages: result.get("messages").cloned().unwrap_or_else(|| json!([])),
        },
        Vec::new(),
        Vec::new(),
        false,
        false,
    ))
}

pub fn load_mcp_db() -> Result<McpServerDb> {
    let settings = resolve_settings()?;
    let store = RuntimeStore::open(&settings.data_dir)?;
    store.import_json_once::<McpServerDb>(
        MCP_SERVERS_NAMESPACE,
        &settings.data_dir.join(MCP_SERVERS_FILE),
    )?;
    Ok(store.get(MCP_SERVERS_NAMESPACE)?.unwrap_or_default())
}

fn save_mcp_db(db: &McpServerDb) -> Result<PathBuf> {
    let settings = resolve_settings()?;
    let store = RuntimeStore::open(&settings.data_dir)?;
    store.put(MCP_SERVERS_NAMESPACE, db)?;
    Ok(store.path().to_path_buf())
}

fn enabled_server(name: &str) -> Result<std::result::Result<McpServerConfig, (String, Blocker)>> {
    if !mcp_enabled()? {
        return Ok(Err((
            "stub_blocked".to_string(),
            Blocker::new(
                "mcp_native_disabled",
                "Native MCP execution is disabled until explicitly enabled in settings.",
                vec![
                    "Run `mom-llama settings update --set mcpNativeEnabled=true --json`."
                        .to_string(),
                ],
            ),
        )));
    }
    let db = load_mcp_db()?;
    let Some(server) = db.servers.into_iter().find(|server| server.name == name) else {
        return Ok(Err((
            "stub_blocked".to_string(),
            Blocker::new(
                "mcp_server_not_found",
                format!("MCP server {name} was not found."),
                vec!["Run `mom-llama mcp list-servers --json`.".to_string()],
            ),
        )));
    };
    if !server.enabled {
        return Ok(Err((
            "stub_blocked".to_string(),
            Blocker::new(
                "mcp_server_disabled",
                format!("MCP server {name} is disabled."),
                vec!["Enable the server with `mcp configure --enabled true`.".to_string()],
            ),
        )));
    }
    Ok(Ok(server))
}

fn execute_mcp_request(server: &McpServerConfig, method: &str, params: Value) -> Result<Value> {
    let timeout_s = resolve_settings()
        .ok()
        .and_then(|settings| upstream_setting_i64(&settings, "mcpRequestTimeoutSeconds"))
        .unwrap_or(30) as f64;
    let mut child = Command::new(&server.command)
        .args(&server.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to start MCP server {}", server.command.display()))?;
    {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("failed to open MCP stdin"))?;
        write_mcp_message(
            &mut stdin,
            &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"mom-llama-lab","version":"0.1.0"}}}),
        )?;
        write_mcp_message(
            &mut stdin,
            &json!({"jsonrpc":"2.0","method":"notifications/initialized","params":{}}),
        )?;
        write_mcp_message(
            &mut stdin,
            &json!({"jsonrpc":"2.0","id":2,"method":method,"params":params}),
        )?;
    }
    let output = command_output_with_timeout_from_child(child, timeout_s)?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_mcp_response(&stdout).with_context(|| format!("failed to parse MCP response: {stdout}"))
}

fn command_output_with_timeout_from_child(
    mut child: std::process::Child,
    timeout_s: f64,
) -> Result<std::process::Output> {
    let pid = child.id();
    let start = std::time::Instant::now();
    let timeout = std::time::Duration::from_secs_f64(timeout_s.max(0.001));
    loop {
        if start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("MCP server pid {pid} timed out after {timeout_s:.3}s");
        }
        if child.try_wait()?.is_some() {
            return child.wait_with_output().map_err(anyhow::Error::new);
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

fn write_mcp_message(writer: &mut impl Write, value: &Value) -> Result<()> {
    let body = serde_json::to_vec(value)?;
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(&body)?;
    writer.flush()?;
    Ok(())
}

fn parse_mcp_response(raw: &str) -> Result<Value> {
    let mut rest = raw;
    while let Some(header_end) = rest.find("\r\n\r\n") {
        let body_start = header_end + 4;
        let headers = &rest[..header_end];
        let Some(length) = headers.lines().find_map(|line| {
            line.strip_prefix("Content-Length:")
                .and_then(|value| value.trim().parse::<usize>().ok())
        }) else {
            break;
        };
        let body_end = body_start + length;
        if rest.len() < body_end {
            break;
        }
        let body = &rest[body_start..body_end];
        let value: Value = serde_json::from_str(body)?;
        if value.get("id").and_then(Value::as_i64) == Some(2) {
            return Ok(value);
        }
        rest = &rest[body_end..];
    }
    serde_json::from_str(raw).map_err(anyhow::Error::new)
}

fn mcp_enabled() -> Result<bool> {
    let settings = resolve_settings()?;
    Ok(settings
        .upstream_settings
        .get("mcpNativeEnabled")
        .and_then(Value::as_bool)
        .unwrap_or(false))
}

fn validate_mcp_command(command: &Path) -> Option<Blocker> {
    if !command.is_absolute() {
        return Some(Blocker::new(
            "mcp_command_not_absolute",
            "MCP server command must be an absolute executable path.",
            vec!["Choose an absolute local executable path.".to_string()],
        ));
    }
    if !command.exists() || !command.is_file() {
        return Some(Blocker::new(
            "mcp_command_invalid",
            format!("MCP server command does not exist: {}.", command.display()),
            vec!["Choose an existing local executable.".to_string()],
        ));
    }
    None
}

fn default_true() -> bool {
    true
}

trait WithResult<T>
where
    T: Serialize,
{
    fn with_result(self, result: T) -> CommandResult<T>;
}

impl<T> WithResult<T> for CommandResult<T>
where
    T: Serialize,
{
    fn with_result(mut self, result: T) -> CommandResult<T> {
        self.result = Some(result);
        self
    }
}
