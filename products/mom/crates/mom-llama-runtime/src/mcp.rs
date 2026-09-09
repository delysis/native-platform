use crate::config::{resolve_settings, upstream_setting_i64};
use crate::operation_scope::OperationScope;
use crate::receipts::{Blocker, CommandResult};
use crate::store::RuntimeStore;
use anyhow::{Context, Result};
#[cfg(any(target_os = "macos", target_os = "linux"))]
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, VecDeque};
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::fs::OpenOptions;
use std::fs::{self, File};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout};
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
#[cfg(any(target_os = "macos", target_os = "linux"))]
use uuid::Uuid;

const MCP_SERVERS_NAMESPACE: &str = "mcp-servers.v2";
const MCP_TOOL_CATALOG_NAMESPACE: &str = "mcp-tool-catalog.v1";
const MCP_PROTOCOL_VERSION: &str = "2024-11-05";
const MAX_MCP_MESSAGE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_MCP_UNRELATED_MESSAGES: usize = 64;
const MAX_MCP_TOOL_CONTENT_ITEMS: usize = 128;
const MAX_CACHED_MCP_TOOLS: usize = 128;
const MAX_CACHED_MCP_TOOL_SCHEMA_BYTES: usize = 64 * 1024;
const MAX_CACHED_MCP_SERVERS: usize = 16;
const MAX_MCP_EXECUTABLE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_MCP_PUMP_READS: usize = 16;
const MAX_MCP_PUMP_BYTES: usize = 128 * 1024;
const MAX_MCP_REQUEST_TIMEOUT_SECONDS: i64 = 120;
#[cfg(any(target_os = "macos", target_os = "linux"))]
const MAX_PERSONA_MCP_EXECUTABLES: usize = 8;
#[cfg(any(target_os = "macos", target_os = "linux"))]
const MAX_PERSONA_MCP_EXECUTABLE_BYTES: u64 = 512 * 1024 * 1024;
#[cfg(any(target_os = "macos", target_os = "linux"))]
const PERSONA_MCP_EXECUTABLES_DIR: &str = "persona-mcp-executables-v1";
#[cfg(any(target_os = "macos", target_os = "linux"))]
const PERSONA_MCP_EXECUTABLES_LOCK: &str = ".persona-mcp-executables-v1.lock";
#[cfg(any(target_os = "macos", target_os = "linux"))]
const MAX_MANAGED_MCP_DIRECTORY_ENTRIES: usize = MAX_PERSONA_MCP_EXECUTABLES * 2;
#[cfg(any(target_os = "macos", target_os = "linux"))]
const MANAGED_MCP_LOCK_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpServerConfig {
    pub name: String,
    pub command: PathBuf,
    #[serde(default)]
    pub executable_sha256: Option<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
struct McpToolCatalogDb {
    #[serde(default)]
    servers: Vec<McpToolCatalogEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct McpToolCatalogEntry {
    server: String,
    server_config_sha256: String,
    frozen_server_config: McpServerConfig,
    frozen_server_config_sha256: String,
    tools: Vec<McpTool>,
}

#[derive(Debug, Clone)]
pub(crate) struct PersonaMcpToolContract {
    pub tool: McpTool,
    pub server_config_sha256: String,
    pub frozen_server_config: McpServerConfig,
    pub frozen_server_config_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpCallToolOutput {
    pub server: String,
    pub tool: String,
    pub content: Value,
}

#[derive(Debug)]
pub(crate) struct McpCallSupervisionError {
    message: String,
    outcome_unknown: bool,
}

impl McpCallSupervisionError {
    fn before_effect(error: impl std::fmt::Display) -> Self {
        Self {
            message: error.to_string(),
            outcome_unknown: false,
        }
    }

    fn after_external_process_spawn(error: impl std::fmt::Display) -> Self {
        Self {
            message: error.to_string(),
            outcome_unknown: true,
        }
    }

    fn at_process_spawn_boundary(error: impl std::fmt::Display, process_spawned: bool) -> Self {
        if process_spawned {
            Self::after_external_process_spawn(error)
        } else {
            Self::before_effect(error)
        }
    }

    pub(crate) const fn outcome_unknown(&self) -> bool {
        self.outcome_unknown
    }
}

impl std::fmt::Display for McpCallSupervisionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for McpCallSupervisionError {}

#[derive(Debug)]
struct McpWriteFailure {
    error: anyhow::Error,
    bytes_written: usize,
}

impl McpWriteFailure {
    #[cfg(test)]
    fn into_before_or_unknown(self) -> McpCallSupervisionError {
        if self.bytes_written == 0 {
            McpCallSupervisionError::before_effect(self.error)
        } else {
            McpCallSupervisionError::after_external_process_spawn(self.error)
        }
    }
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
    if let Some(blocker) = mcp_platform_blocker() {
        return Ok(CommandResult::blocked(
            "mom_llama.mcp_status",
            "blocked_platform_unsupported",
            blocker,
        )
        .with_result(result));
    }
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
    let (identity, _) = read_mcp_command_identity(&command, false)?;
    let mut db = load_mcp_db()?;
    let config = McpServerConfig {
        name: name.trim().to_string(),
        command: identity.canonical_path,
        executable_sha256: Some(identity.sha256),
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

pub fn mcp_list_tools_in_scope(
    scope: &OperationScope,
    server_name: &str,
) -> Result<CommandResult<Vec<McpTool>>> {
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
    let response = execute_mcp_request(scope, &server, "tools/list", json!({}))?;
    let tools = parse_mcp_tools(response)?;
    let changed_paths = cache_persona_mcp_tool_catalog(&server, &tools)?
        .into_iter()
        .map(|path| path.display().to_string())
        .collect();
    Ok(CommandResult::passed(
        "mom_llama.mcp_list_tools",
        "host_integrated",
        tools,
        changed_paths,
        Vec::new(),
        false,
        false,
    ))
}

pub(crate) fn cached_persona_mcp_tool_contract(
    server_name: &str,
    tool_name: &str,
) -> Result<std::result::Result<PersonaMcpToolContract, (String, Blocker)>> {
    if let Some(blocker) = mcp_platform_blocker() {
        return Ok(Err(("blocked_platform_unsupported".to_string(), blocker)));
    }
    if !mcp_enabled()? {
        return Ok(Err((
            "stub_blocked".to_string(),
            Blocker::new(
                "mcp_native_disabled",
                "Native MCP execution is disabled until explicitly enabled in settings.",
                vec![
                    "Enable MCP only after reviewing the configured process authority.".to_string(),
                ],
            ),
        )));
    }
    let server_db = load_mcp_db()?;
    let Some(server) = server_db
        .servers
        .iter()
        .find(|server| server.name == server_name && server.enabled)
    else {
        return Ok(Err((
            "stub_blocked".to_string(),
            Blocker::new(
                "mcp_server_not_found",
                format!("MCP server {server_name} is missing or disabled."),
                vec!["Review configured MCP servers.".to_string()],
            ),
        )));
    };
    if !server.args.is_empty() {
        return Ok(Err((
            "blocked_exact_executable_required".to_string(),
            Blocker::new(
                "mention_tool_server_not_self_contained",
                "Persona approvals require one reviewed direct native MCP executable with no arguments.",
                vec![
                    "Configure a direct native MCP executable and refresh its tool catalog."
                        .to_string(),
                ],
            ),
        )));
    }
    let server_config_sha256 = match exact_mcp_server_config_sha256(server) {
        Ok(identity) => identity,
        Err(error) => {
            return Ok(Err((
                "blocked_exact_executable_required".to_string(),
                Blocker::new(
                    "mention_tool_server_identity_failed",
                    error.to_string(),
                    vec![
                        "Configure a direct native MCP executable and refresh its tool catalog."
                            .to_string(),
                    ],
                ),
            )));
        }
    };
    let settings = resolve_settings()?;
    let store = RuntimeStore::open(&settings.data_dir)?;
    let catalog = store
        .get::<McpToolCatalogDb>(MCP_TOOL_CATALOG_NAMESPACE)?
        .unwrap_or_default();
    let Some(entry) = catalog.servers.into_iter().find(|entry| {
        entry.server == server_name && entry.server_config_sha256 == server_config_sha256
    }) else {
        return Ok(Err((
            "blocked_tool_catalog_required".to_string(),
            Blocker::new(
                "mention_tool_catalog_missing",
                format!(
                    "The exact tool catalog for MCP server `{server_name}` has not been reviewed."
                ),
                vec![format!(
                    "Run `mom-llama mcp list-tools --server {server_name} --json` before attaching this tool."
                )],
            ),
        )));
    };
    if let Err(error) = validate_frozen_mcp_server(
        &entry.frozen_server_config,
        &settings.data_dir,
        &entry.frozen_server_config_sha256,
    ) {
        return Ok(Err((
            "blocked_tool_catalog_required".to_string(),
            Blocker::new(
                "mention_tool_catalog_executable_changed",
                error.to_string(),
                vec!["Refresh the exact MCP tool catalog.".to_string()],
            ),
        )));
    }
    let matches = entry
        .tools
        .into_iter()
        .filter(|tool| tool.name == tool_name)
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return Ok(Err((
            "blocked_tool_catalog_required".to_string(),
            Blocker::new(
                "mention_tool_catalog_mismatch",
                format!(
                    "The reviewed catalog does not contain exactly one tool named `{tool_name}`."
                ),
                vec![
                    "Refresh the exact MCP tool catalog and review the Persona binding."
                        .to_string(),
                ],
            ),
        )));
    }
    Ok(Ok(PersonaMcpToolContract {
        tool: matches.into_iter().next().expect("one exact tool"),
        server_config_sha256,
        frozen_server_config: entry.frozen_server_config,
        frozen_server_config_sha256: entry.frozen_server_config_sha256,
    }))
}

fn cache_persona_mcp_tool_catalog(
    server: &McpServerConfig,
    tools: &[McpTool],
) -> Result<Vec<PathBuf>> {
    if !server.args.is_empty() {
        return Ok(Vec::new());
    }
    let Ok(server_config_sha256) = exact_mcp_server_config_sha256(server) else {
        return Ok(Vec::new());
    };
    if tools.len() > MAX_CACHED_MCP_TOOLS {
        anyhow::bail!(
            "MCP tool catalog exceeds the {} tool limit",
            MAX_CACHED_MCP_TOOLS
        );
    }
    let encoded = serde_json::to_vec(tools)?;
    if encoded.len() as u64 > MAX_MCP_MESSAGE_BYTES {
        anyhow::bail!(
            "MCP tool catalog exceeds the {} byte limit",
            MAX_MCP_MESSAGE_BYTES
        );
    }
    let settings = resolve_settings()?;
    let store = RuntimeStore::open(&settings.data_dir)?;
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (server, tools, server_config_sha256, settings, store);
        anyhow::bail!("reviewed Persona MCP executable staging is unsupported on this platform")
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    cache_persona_mcp_tool_catalog_with_persistence(
        &store,
        &settings.data_dir,
        server,
        tools,
        &server_config_sha256,
        persist_persona_mcp_tool_catalog,
    )
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn persist_persona_mcp_tool_catalog(
    store: &RuntimeStore,
    entry: McpToolCatalogEntry,
) -> Result<()> {
    persist_persona_mcp_tool_catalog_with_hook(store, entry, |_| Ok(()))
}

#[cfg(any(test, target_os = "macos", target_os = "linux"))]
fn persist_persona_mcp_tool_catalog_with_hook(
    store: &RuntimeStore,
    entry: McpToolCatalogEntry,
    before_update: impl FnOnce(&McpToolCatalogDb) -> Result<()>,
) -> Result<()> {
    store.mutate(
        MCP_TOOL_CATALOG_NAMESPACE,
        McpToolCatalogDb::default,
        |catalog| {
            before_update(catalog)?;
            catalog
                .servers
                .retain(|candidate| candidate.server != entry.server);
            if catalog.servers.len() >= MAX_CACHED_MCP_SERVERS {
                anyhow::bail!(
                    "MCP tool catalog exceeds the {} reviewed server limit",
                    MAX_CACHED_MCP_SERVERS
                );
            }
            catalog.servers.push(entry);
            catalog
                .servers
                .sort_by(|left, right| left.server.cmp(&right.server));
            Ok(())
        },
    )
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn cache_persona_mcp_tool_catalog_with_persistence(
    store: &RuntimeStore,
    data_dir: &Path,
    server: &McpServerConfig,
    tools: &[McpTool],
    server_config_sha256: &str,
    persist_catalog: impl FnOnce(&RuntimeStore, McpToolCatalogEntry) -> Result<()>,
) -> Result<Vec<PathBuf>> {
    let mut managed_store = lock_managed_mcp_store_for_write(data_dir)?;
    let (frozen_server_config, frozen_server_config_sha256, newly_staged) =
        stage_mcp_server_for_persona_locked(server, server_config_sha256, &mut managed_store)?;
    let entry = McpToolCatalogEntry {
        server: server.name.clone(),
        server_config_sha256: server_config_sha256.to_string(),
        frozen_server_config: frozen_server_config.clone(),
        frozen_server_config_sha256: frozen_server_config_sha256.clone(),
        tools: tools.to_vec(),
    };
    if let Err(persist_error) = persist_catalog(store, entry) {
        let rollback_error = rollback_new_unreferenced_managed_mcp_executable(
            store,
            &managed_store.dir,
            &frozen_server_config.command,
            newly_staged,
        )
        .err();
        let seal_error = managed_store.seal().err();
        drop(managed_store);
        return match (rollback_error, seal_error) {
            (None, None) => Err(persist_error),
            (rollback_error, seal_error) => Err(anyhow::anyhow!(
                "{persist_error:#}; managed MCP rollback error: {}; managed MCP seal error: {}",
                rollback_error
                    .map(|error| format!("{error:#}"))
                    .unwrap_or_else(|| "none".to_string()),
                seal_error
                    .map(|error| format!("{error:#}"))
                    .unwrap_or_else(|| "none".to_string())
            )),
        };
    }
    managed_store.seal()?;
    drop(managed_store);
    validate_frozen_mcp_server(
        &frozen_server_config,
        data_dir,
        &frozen_server_config_sha256,
    )?;
    if exact_mcp_server_config_sha256(server)? != server_config_sha256 {
        anyhow::bail!("configured MCP executable identity drifted while its catalog was reviewed");
    }
    Ok(vec![
        frozen_server_config.command,
        store.path().to_path_buf(),
    ])
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn rollback_new_unreferenced_managed_mcp_executable(
    store: &RuntimeStore,
    managed_dir: &Path,
    managed_path: &Path,
    newly_staged: bool,
) -> Result<()> {
    if !newly_staged {
        return Ok(());
    }
    let referenced = store
        .get::<McpToolCatalogDb>(MCP_TOOL_CATALOG_NAMESPACE)?
        .unwrap_or_default()
        .servers
        .iter()
        .any(|entry| entry.frozen_server_config.command == managed_path);
    if referenced {
        return Ok(());
    }
    match fs::remove_file(managed_path) {
        Ok(()) => File::open(managed_dir)?.sync_all()?,
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn parse_mcp_tools(response: Value) -> Result<Vec<McpTool>> {
    let raw_tools = response
        .pointer("/result/tools")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("MCP tools/list result omitted its tools array"))?;
    if raw_tools.len() > MAX_CACHED_MCP_TOOLS {
        anyhow::bail!(
            "MCP tools/list exceeds the {} tool limit",
            MAX_CACHED_MCP_TOOLS
        );
    }
    let mut names = BTreeSet::new();
    let mut tools = Vec::with_capacity(raw_tools.len());
    for raw_tool in raw_tools {
        let object = raw_tool
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("MCP tool entry must be an object"))?;
        let name = object
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("MCP tool entry has no nonempty name"))?
            .to_string();
        if !names.insert(name.clone()) {
            anyhow::bail!("MCP tools/list contains duplicate tool name `{name}`");
        }
        let description = match object.get("description") {
            None => None,
            Some(Value::String(description)) => Some(description.clone()),
            Some(_) => anyhow::bail!("MCP tool `{name}` description must be a string"),
        };
        let input_schema = object
            .get("inputSchema")
            .cloned()
            .unwrap_or_else(|| json!({"type":"object"}));
        if !input_schema.is_object() {
            anyhow::bail!("MCP tool `{name}` inputSchema must be an object");
        }
        if serde_json::to_vec(&input_schema)?.len() > MAX_CACHED_MCP_TOOL_SCHEMA_BYTES {
            anyhow::bail!(
                "MCP tool `{name}` inputSchema exceeds the {} byte limit",
                MAX_CACHED_MCP_TOOL_SCHEMA_BYTES
            );
        }
        tools.push(McpTool {
            name,
            description,
            input_schema,
        });
    }
    Ok(tools)
}

pub fn mcp_call_tool_in_scope(
    scope: &OperationScope,
    server_name: &str,
    tool_name: &str,
    arguments: Value,
) -> Result<CommandResult<McpCallToolOutput>> {
    mcp_call_tool_impl(scope, server_name, tool_name, arguments, None)
}

pub(crate) fn mcp_call_tool_supervised(
    scope: &OperationScope,
    server_name: &str,
    tool_name: &str,
    arguments: Value,
    should_cancel: &dyn Fn() -> bool,
) -> Result<CommandResult<McpCallToolOutput>> {
    mcp_call_tool_impl(
        scope,
        server_name,
        tool_name,
        arguments,
        Some(should_cancel),
    )
}

pub(crate) fn mcp_call_tool_supervised_with_config(
    scope: &OperationScope,
    server: &McpServerConfig,
    tool_name: &str,
    arguments: Value,
    expected_server_config_sha256: &str,
    expected_tool_schema_sha256: &str,
    should_cancel: &dyn Fn() -> bool,
) -> std::result::Result<CommandResult<McpCallToolOutput>, McpCallSupervisionError> {
    let operation = scope
        .register_mcp()
        .map_err(McpCallSupervisionError::before_effect)?;
    let scoped_cancel = || operation.cancellation_requested() || should_cancel();
    if !server.enabled {
        return Err(McpCallSupervisionError::before_effect(
            "frozen MCP server is disabled",
        ));
    }
    if tool_name.trim().is_empty() {
        return Err(McpCallSupervisionError::before_effect(
            "frozen MCP tool name is empty",
        ));
    }
    let actual_server_config_sha256 =
        exact_mcp_server_config_sha256(server).map_err(McpCallSupervisionError::before_effect)?;
    if actual_server_config_sha256 != expected_server_config_sha256 {
        return Err(McpCallSupervisionError::before_effect(
            "frozen MCP executable identity changed at admission",
        ));
    }
    let response = execute_mcp_effect_request_supervised(
        server,
        tool_name,
        arguments,
        expected_tool_schema_sha256,
        &scoped_cancel,
    )?;
    match response {
        McpEffectTerminal::Success(content) => Ok(CommandResult::passed(
            "mom_llama.mcp_call_tool",
            "host_integrated",
            McpCallToolOutput {
                server: server.name.clone(),
                tool: tool_name.to_string(),
                content,
            },
            Vec::new(),
            Vec::new(),
            false,
            false,
        )),
        McpEffectTerminal::Rejected(error) => Ok(CommandResult::blocked_with_evidence(
            "mom_llama.mcp_call_tool",
            "host_integrated",
            Blocker::new(
                "mcp_tool_call_rejected",
                format!(
                    "The reviewed configured MCP process rejected the tool call: {}",
                    rpc_error_message(&error)
                ),
                vec!["Review the persisted tool receipt before retrying.".to_string()],
            ),
            Vec::new(),
            vec!["external_effect_outcome:known".to_string()],
            false,
            false,
        )),
    }
}

fn mcp_call_tool_impl(
    scope: &OperationScope,
    server_name: &str,
    tool_name: &str,
    arguments: Value,
    should_cancel: Option<&dyn Fn() -> bool>,
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
    let operation = scope.register_mcp()?;
    let scoped_cancel = || {
        operation.cancellation_requested()
            || should_cancel.is_some_and(|should_cancel| should_cancel())
    };
    mcp_call_tool_with_server(&server, tool_name, arguments, Some(&scoped_cancel))
}

fn mcp_call_tool_with_server(
    server: &McpServerConfig,
    tool_name: &str,
    arguments: Value,
    should_cancel: Option<&dyn Fn() -> bool>,
) -> Result<CommandResult<McpCallToolOutput>> {
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
    let response = execute_mcp_request_supervised(
        server,
        "tools/call",
        json!({
            "name": tool_name,
            "arguments": arguments,
        }),
        should_cancel,
    )?;
    Ok(CommandResult::passed(
        "mom_llama.mcp_call_tool",
        "host_integrated",
        McpCallToolOutput {
            server: server.name.clone(),
            tool: tool_name.to_string(),
            content: response.get("result").cloned().unwrap_or(response),
        },
        Vec::new(),
        Vec::new(),
        false,
        false,
    ))
}

pub fn mcp_list_resources_in_scope(
    scope: &OperationScope,
    server_name: &str,
) -> Result<CommandResult<Vec<McpResource>>> {
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
    let response = execute_mcp_request(scope, &server, "resources/list", json!({}))?;
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

pub fn mcp_read_resource_in_scope(
    scope: &OperationScope,
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
    let response = execute_mcp_request(scope, &server, "resources/read", json!({ "uri": uri }))?;
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

pub fn mcp_list_prompts_in_scope(
    scope: &OperationScope,
    server_name: &str,
) -> Result<CommandResult<Vec<McpPrompt>>> {
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
    let response = execute_mcp_request(scope, &server, "prompts/list", json!({}))?;
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

pub fn mcp_get_prompt_in_scope(
    scope: &OperationScope,
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
        scope,
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
    Ok(store.get(MCP_SERVERS_NAMESPACE)?.unwrap_or_default())
}

fn save_mcp_db(db: &McpServerDb) -> Result<PathBuf> {
    let settings = resolve_settings()?;
    let store = RuntimeStore::open(&settings.data_dir)?;
    store.put(MCP_SERVERS_NAMESPACE, db)?;
    Ok(store.path().to_path_buf())
}

fn enabled_server(name: &str) -> Result<std::result::Result<McpServerConfig, (String, Blocker)>> {
    if let Some(blocker) = mcp_platform_blocker() {
        return Ok(Err(("blocked_platform_unsupported".to_string(), blocker)));
    }
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

fn execute_mcp_request(
    scope: &OperationScope,
    server: &McpServerConfig,
    method: &str,
    params: Value,
) -> Result<Value> {
    let operation = scope.register_mcp()?;
    execute_mcp_request_supervised(
        server,
        method,
        params,
        Some(&|| operation.cancellation_requested()),
    )
}

#[derive(Debug)]
enum McpRpcTerminal {
    Result(Value),
    Error(Value),
}

#[derive(Debug)]
enum McpEffectTerminal {
    Success(Value),
    Rejected(Value),
}

struct McpStdioSession {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: ChildStdout,
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    _managed_store_guard: Option<ManagedMcpReadGuard>,
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    _executable_guard: Option<File>,
    pending: Vec<u8>,
    responses: VecDeque<std::result::Result<Value, String>>,
    stdout_closed: bool,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
struct McpChildSetupGuard(Option<Child>);

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl McpChildSetupGuard {
    fn child_mut(&mut self) -> &mut Child {
        self.0.as_mut().expect("MCP setup child")
    }

    fn disarm(mut self) -> Child {
        self.0.take().expect("MCP setup child")
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl Drop for McpChildSetupGuard {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            terminate_mcp_process_group(child);
        }
    }
}

impl McpStdioSession {
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    fn spawn(server: &McpServerConfig) -> Result<Self> {
        Self::spawn_observed(server, None)
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    fn spawn_observed(
        server: &McpServerConfig,
        process_spawned: Option<&mut bool>,
    ) -> Result<Self> {
        let _ = server;
        if let Some(process_spawned) = process_spawned {
            *process_spawned = false;
        }
        anyhow::bail!("joined MCP process-group supervision is not implemented on this platform")
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn spawn(server: &McpServerConfig) -> Result<Self> {
        Self::spawn_observed(server, None)
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn spawn_observed(
        server: &McpServerConfig,
        process_spawned: Option<&mut bool>,
    ) -> Result<Self> {
        if let Some(blocker) = validate_mcp_command(&server.command) {
            anyhow::bail!("{}", blocker.message);
        }
        let managed_store_guard = managed_mcp_read_guard(&server.command)?;
        let (expected_identity, executable_guard) =
            read_mcp_command_identity(&server.command, managed_store_guard.is_some())?;
        if server.executable_sha256.as_deref() != Some(expected_identity.sha256.as_str()) {
            anyhow::bail!(
                "MCP executable differs from its saved configuration or has no saved identity; review and configure it again"
            );
        }
        let command_path = expected_identity.canonical_path.clone();
        let working_dir = command_path.parent().ok_or_else(|| {
            anyhow::anyhow!("MCP server command has no deterministic parent directory")
        })?;
        let mut command = Command::new(&command_path);
        command
            .args(&server.args)
            .env_clear()
            .current_dir(working_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        use std::os::unix::process::CommandExt;
        command.process_group(0);
        let child = command
            .spawn()
            .with_context(|| format!("failed to start MCP server {}", command_path.display()))?;
        if let Some(process_spawned) = process_spawned {
            // This is the evidence boundary. Once the OS reports a successful
            // spawn, the unsandboxed process may already have exercised any
            // authority available to the user's account. Every later failure
            // is therefore outcome-unknown even if tools/call received no byte.
            *process_spawned = true;
        }
        let mut child = McpChildSetupGuard(Some(child));
        let stdin = child
            .child_mut()
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("failed to open MCP stdin"))?;
        let stdout = child
            .child_mut()
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("failed to open MCP stdout"))?;
        use nix::fcntl::{FcntlArg, OFlag, fcntl};
        use std::os::fd::AsFd;
        for descriptor in [stdin.as_fd(), stdout.as_fd()] {
            let current = fcntl(descriptor, FcntlArg::F_GETFL)?;
            let flags = OFlag::from_bits_truncate(current) | OFlag::O_NONBLOCK;
            fcntl(descriptor, FcntlArg::F_SETFL(flags))?;
        }
        {
            let (actual_identity, _) =
                read_mcp_command_identity(&server.command, managed_store_guard.is_some())?;
            if actual_identity != expected_identity {
                anyhow::bail!(
                    "MCP pathname identity drifted across process spawn; the executed inode is not asserted"
                );
            }
        }
        let child = child.disarm();
        Ok(Self {
            child,
            stdin: Some(stdin),
            stdout,
            _managed_store_guard: managed_store_guard,
            _executable_guard: Some(executable_guard),
            pending: Vec::new(),
            responses: VecDeque::new(),
            stdout_closed: false,
        })
    }

    fn send(
        &mut self,
        value: &Value,
        deadline: Instant,
        should_cancel: Option<&dyn Fn() -> bool>,
    ) -> Result<()> {
        let message = serialize_mcp_message(value)?;
        write_mcp_bytes_bounded(
            self.stdin
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("MCP stdin is closed"))?,
            &message,
            deadline,
            should_cancel,
        )
        .map_err(|failure| failure.error)
    }

    fn send_effect(
        &mut self,
        params: Value,
        deadline: Instant,
        should_cancel: &dyn Fn() -> bool,
    ) -> std::result::Result<(), McpCallSupervisionError> {
        let message = serialize_mcp_message(
            &json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":params}),
        )
        .map_err(McpCallSupervisionError::before_effect)?;
        write_mcp_bytes_bounded(
            self.stdin
                .as_mut()
                .ok_or_else(|| McpCallSupervisionError::before_effect("MCP stdin is closed"))?,
            &message,
            deadline,
            Some(should_cancel),
        )
        .map_err(|failure| {
            let dispatch_detail = if failure.bytes_written == 0 {
                "no tools/call bytes were observed written"
            } else {
                "a partial tools/call frame was written"
            };
            McpCallSupervisionError::after_external_process_spawn(format!(
                "{dispatch_detail}: {}",
                failure.error
            ))
        })
    }

    fn receive(
        &mut self,
        expected_id: u64,
        deadline: Instant,
        should_cancel: Option<&dyn Fn() -> bool>,
    ) -> Result<McpRpcTerminal> {
        let mut unrelated = 0usize;
        loop {
            if cancellation_requested(should_cancel) {
                anyhow::bail!("MCP request was cancelled while awaiting response {expected_id}");
            }
            let now = Instant::now();
            if now >= deadline {
                anyhow::bail!("MCP request timed out while awaiting response {expected_id}");
            }
            self.pump_stdout()?;
            let Some(response) = self.responses.pop_front() else {
                if self.stdout_closed {
                    anyhow::bail!("MCP server exited without exact response id {expected_id}");
                }
                std::thread::sleep(
                    deadline
                        .saturating_duration_since(now)
                        .min(Duration::from_millis(5)),
                );
                continue;
            };
            let value = response.map_err(anyhow::Error::msg)?;
            match exact_rpc_terminal(value, expected_id)? {
                Some(terminal) => return Ok(terminal),
                None => {
                    unrelated = unrelated.saturating_add(1);
                    if unrelated > MAX_MCP_UNRELATED_MESSAGES {
                        anyhow::bail!(
                            "MCP server exceeded the response arbitration limit before id {expected_id}"
                        );
                    }
                }
            }
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    fn pump_stdout(&mut self) -> Result<()> {
        anyhow::bail!("nonblocking joined MCP stdout is unavailable on this platform");
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn pump_stdout(&mut self) -> Result<()> {
        let mut buffer = [0_u8; 8192];
        let mut reads = 0usize;
        let mut bytes = 0usize;
        while reads < MAX_MCP_PUMP_READS && bytes < MAX_MCP_PUMP_BYTES {
            match self.stdout.read(&mut buffer) {
                Ok(0) => {
                    self.stdout_closed = true;
                    if !self.pending.is_empty() {
                        self.responses.push_back(Err(
                            "MCP stdout ended with an unterminated JSON message".to_string(),
                        ));
                        self.pending.clear();
                    }
                    break;
                }
                Ok(read) => {
                    reads = reads.saturating_add(1);
                    bytes = bytes.saturating_add(read);
                    self.pending.extend_from_slice(&buffer[..read]);
                    while let Some(end) = self.pending.iter().position(|byte| *byte == b'\n') {
                        let mut line = self.pending.drain(..=end).collect::<Vec<_>>();
                        while matches!(line.last(), Some(b'\n' | b'\r')) {
                            line.pop();
                        }
                        if line.is_empty() {
                            continue;
                        }
                        if line.len() as u64 > MAX_MCP_MESSAGE_BYTES {
                            self.responses.push_back(Err(format!(
                                "MCP response exceeds the {} byte limit",
                                MAX_MCP_MESSAGE_BYTES
                            )));
                            self.stdout_closed = true;
                            return Ok(());
                        }
                        let value = serde_json::from_slice(&line)
                            .map_err(|error| format!("MCP response is not one JSON line: {error}"));
                        self.responses.push_back(value);
                        if self.responses.len() > MAX_MCP_UNRELATED_MESSAGES + 1 {
                            self.responses.push_back(Err(format!(
                                "MCP server exceeded the {} message arbitration limit",
                                MAX_MCP_UNRELATED_MESSAGES
                            )));
                            self.stdout_closed = true;
                            return Ok(());
                        }
                    }
                    if self.pending.len() as u64 > MAX_MCP_MESSAGE_BYTES {
                        self.responses.push_back(Err(format!(
                            "MCP response exceeds the {} byte limit",
                            MAX_MCP_MESSAGE_BYTES
                        )));
                        self.stdout_closed = true;
                        return Ok(());
                    }
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) => anyhow::bail!("MCP stdout read failed: {error}"),
            }
        }
        Ok(())
    }

    fn initialize(
        &mut self,
        deadline: Instant,
        should_cancel: Option<&dyn Fn() -> bool>,
    ) -> Result<()> {
        self.send(
            &json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": MCP_PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {"name": "mom-llama", "version": "0.1.0"}
                }
            }),
            deadline,
            should_cancel,
        )?;
        let result = match self.receive(1, deadline, should_cancel)? {
            McpRpcTerminal::Result(result) => result,
            McpRpcTerminal::Error(error) => {
                anyhow::bail!("MCP initialize rejected: {}", rpc_error_message(&error))
            }
        };
        if result.get("protocolVersion").and_then(Value::as_str) != Some(MCP_PROTOCOL_VERSION) {
            anyhow::bail!("MCP server did not negotiate protocol version {MCP_PROTOCOL_VERSION}");
        }
        let server_info = result
            .get("serverInfo")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow::anyhow!("MCP initialize result omitted serverInfo"))?;
        if server_info
            .get("name")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
            || server_info
                .get("version")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
        {
            anyhow::bail!("MCP serverInfo must contain nonempty name and version");
        }
        if result
            .pointer("/capabilities/tools")
            .and_then(Value::as_object)
            .is_none()
        {
            anyhow::bail!("MCP server did not negotiate the tools capability");
        }
        self.send(
            &json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized",
                "params": {}
            }),
            deadline,
            should_cancel,
        )
    }
}

impl Drop for McpStdioSession {
    fn drop(&mut self) {
        self.stdin.take();
        terminate_mcp_process_group(&mut self.child);
    }
}

fn terminate_mcp_process_group(child: &mut Child) {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    if let Ok(pid) = i32::try_from(child.id()) {
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(-pid),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn execute_mcp_request_supervised(
    server: &McpServerConfig,
    method: &str,
    params: Value,
    should_cancel: Option<&dyn Fn() -> bool>,
) -> Result<Value> {
    if cancellation_requested(should_cancel) {
        anyhow::bail!("MCP request was cancelled before server spawn");
    }
    let deadline = mcp_request_deadline()?;
    let mut session = McpStdioSession::spawn(server)?;
    session.initialize(deadline, should_cancel)?;
    if cancellation_requested(should_cancel) {
        anyhow::bail!("MCP request was cancelled before request dispatch");
    }
    session.send(
        &json!({"jsonrpc":"2.0","id":2,"method":method,"params":params}),
        deadline,
        should_cancel,
    )?;
    match session.receive(2, deadline, should_cancel)? {
        McpRpcTerminal::Result(result) => Ok(json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": result
        })),
        McpRpcTerminal::Error(error) => {
            anyhow::bail!("MCP request rejected: {}", rpc_error_message(&error))
        }
    }
}

fn execute_mcp_effect_request_supervised(
    server: &McpServerConfig,
    tool_name: &str,
    arguments: Value,
    expected_tool_schema_sha256: &str,
    should_cancel: &dyn Fn() -> bool,
) -> std::result::Result<McpEffectTerminal, McpCallSupervisionError> {
    if cancellation_requested(Some(should_cancel)) {
        return Err(McpCallSupervisionError::before_effect(
            "MCP tool call was cancelled before server spawn",
        ));
    }
    let deadline = mcp_request_deadline().map_err(McpCallSupervisionError::before_effect)?;
    let mut process_spawned = false;
    let mut session =
        McpStdioSession::spawn_observed(server, Some(&mut process_spawned)).map_err(|error| {
            McpCallSupervisionError::at_process_spawn_boundary(error, process_spawned)
        })?;
    session
        .initialize(deadline, Some(should_cancel))
        .map_err(McpCallSupervisionError::after_external_process_spawn)?;
    session
        .send(
            &json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
            deadline,
            Some(should_cancel),
        )
        .map_err(McpCallSupervisionError::after_external_process_spawn)?;
    let tools = match session
        .receive(2, deadline, Some(should_cancel))
        .map_err(McpCallSupervisionError::after_external_process_spawn)?
    {
        McpRpcTerminal::Result(result) => parse_mcp_tools(json!({"result": result}))
            .map_err(McpCallSupervisionError::after_external_process_spawn)?,
        McpRpcTerminal::Error(error) => {
            return Err(McpCallSupervisionError::after_external_process_spawn(
                format!("MCP tools/list rejected: {}", rpc_error_message(&error)),
            ));
        }
    };
    let matching = tools
        .iter()
        .filter(|tool| tool.name == tool_name)
        .collect::<Vec<_>>();
    if matching.len() != 1 {
        return Err(McpCallSupervisionError::after_external_process_spawn(
            format!("MCP server did not advertise exactly one tool named {tool_name}"),
        ));
    }
    let schema_sha256 = format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&matching[0].input_schema)
                .map_err(McpCallSupervisionError::after_external_process_spawn)?
        )
    );
    if schema_sha256 != expected_tool_schema_sha256 {
        return Err(McpCallSupervisionError::after_external_process_spawn(
            "MCP tool schema changed in the effect session",
        ));
    }
    if cancellation_requested(Some(should_cancel)) {
        return Err(McpCallSupervisionError::after_external_process_spawn(
            "MCP tool call was cancelled after external process spawn and before tools/call dispatch",
        ));
    }

    // Process spawn is already the unknown-outcome boundary. A complete exact
    // id=3 terminal is the only observation that restores a known outcome.
    session.send_effect(
        json!({"name": tool_name, "arguments": arguments}),
        deadline,
        should_cancel,
    )?;
    match session
        .receive(3, deadline, Some(should_cancel))
        .map_err(McpCallSupervisionError::after_external_process_spawn)?
    {
        McpRpcTerminal::Result(result) => validate_mcp_call_tool_result(result)
            .map_err(McpCallSupervisionError::after_external_process_spawn),
        McpRpcTerminal::Error(error) => Ok(McpEffectTerminal::Rejected(error)),
    }
}

fn validate_mcp_call_tool_result(result: Value) -> Result<McpEffectTerminal> {
    let object = result
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("MCP tools/call result must be an object"))?;
    let content = object
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("MCP tools/call result must contain a content array"))?;
    if content.len() > MAX_MCP_TOOL_CONTENT_ITEMS {
        anyhow::bail!(
            "MCP tools/call result exceeds the {} content item limit",
            MAX_MCP_TOOL_CONTENT_ITEMS
        );
    }
    for item in content {
        let item = item
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("MCP tool content item must be an object"))?;
        if item.get("type").and_then(Value::as_str).is_none() {
            anyhow::bail!("MCP tool content item must have a string type");
        }
    }
    let is_error = match object.get("isError") {
        None => false,
        Some(Value::Bool(value)) => *value,
        Some(_) => anyhow::bail!("MCP tools/call isError must be boolean"),
    };
    if is_error {
        Ok(McpEffectTerminal::Rejected(json!({
            "code": -32000,
            "message": "MCP tool returned isError=true",
            "data": result
        })))
    } else {
        Ok(McpEffectTerminal::Success(result))
    }
}

#[cfg(test)]
fn write_mcp_effect_message(
    writer: &mut impl Write,
    params: Value,
) -> std::result::Result<(), McpCallSupervisionError> {
    let message = serialize_mcp_message(
        &json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":params}),
    )
    .map_err(McpCallSupervisionError::before_effect)?;
    write_mcp_bytes_counted(writer, &message).map_err(McpWriteFailure::into_before_or_unknown)
}

fn cancellation_requested(should_cancel: Option<&dyn Fn() -> bool>) -> bool {
    should_cancel.is_some_and(|should_cancel| should_cancel())
}

fn serialize_mcp_message(value: &Value) -> Result<Vec<u8>> {
    let mut encoded = serde_json::to_vec(value)?;
    let encoded_len = u64::try_from(encoded.len())?;
    if encoded_len >= MAX_MCP_MESSAGE_BYTES {
        anyhow::bail!(
            "MCP request exceeds the {} byte limit",
            MAX_MCP_MESSAGE_BYTES
        );
    }
    encoded.push(b'\n');
    Ok(encoded)
}

#[cfg(test)]
fn write_mcp_bytes_counted(
    writer: &mut impl Write,
    bytes: &[u8],
) -> std::result::Result<(), McpWriteFailure> {
    let mut written = 0usize;
    while written < bytes.len() {
        match writer.write(&bytes[written..]) {
            Ok(0) => {
                return Err(McpWriteFailure {
                    error: std::io::Error::new(
                        ErrorKind::WriteZero,
                        "MCP stdin stopped accepting bytes",
                    )
                    .into(),
                    bytes_written: written,
                });
            }
            Ok(count) => written = written.saturating_add(count),
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) => {
                return Err(McpWriteFailure {
                    error: error.into(),
                    bytes_written: written,
                });
            }
        }
    }
    writer.flush().map_err(|error| McpWriteFailure {
        error: error.into(),
        bytes_written: written,
    })
}

fn write_mcp_bytes_bounded(
    writer: &mut impl Write,
    bytes: &[u8],
    deadline: Instant,
    should_cancel: Option<&dyn Fn() -> bool>,
) -> std::result::Result<(), McpWriteFailure> {
    let mut written = 0usize;
    while written < bytes.len() {
        if cancellation_requested(should_cancel) {
            return Err(McpWriteFailure {
                error: anyhow::anyhow!("MCP request was cancelled while writing stdin"),
                bytes_written: written,
            });
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(McpWriteFailure {
                error: anyhow::anyhow!("MCP request timed out while writing stdin"),
                bytes_written: written,
            });
        }
        match writer.write(&bytes[written..]) {
            Ok(0) => {
                return Err(McpWriteFailure {
                    error: std::io::Error::new(
                        ErrorKind::WriteZero,
                        "MCP stdin stopped accepting bytes",
                    )
                    .into(),
                    bytes_written: written,
                });
            }
            Ok(count) => written = written.saturating_add(count),
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                std::thread::sleep(
                    deadline
                        .saturating_duration_since(now)
                        .min(Duration::from_millis(5)),
                );
            }
            Err(error) => {
                return Err(McpWriteFailure {
                    error: error.into(),
                    bytes_written: written,
                });
            }
        }
    }
    loop {
        if cancellation_requested(should_cancel) {
            return Err(McpWriteFailure {
                error: anyhow::anyhow!("MCP request was cancelled while flushing stdin"),
                bytes_written: written,
            });
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(McpWriteFailure {
                error: anyhow::anyhow!("MCP request timed out while flushing stdin"),
                bytes_written: written,
            });
        }
        match writer.flush() {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                std::thread::sleep(
                    deadline
                        .saturating_duration_since(now)
                        .min(Duration::from_millis(5)),
                );
            }
            Err(error) => {
                return Err(McpWriteFailure {
                    error: error.into(),
                    bytes_written: written,
                });
            }
        }
    }
}

fn exact_rpc_terminal(value: Value, expected_id: u64) -> Result<Option<McpRpcTerminal>> {
    let Some(id) = value.get("id") else {
        return Ok(None);
    };
    if id.as_u64() != Some(expected_id) {
        return Ok(None);
    }
    if value.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        anyhow::bail!("MCP response id {expected_id} is not JSON-RPC 2.0");
    }
    match (value.get("result"), value.get("error")) {
        (Some(result), None) => Ok(Some(McpRpcTerminal::Result(result.clone()))),
        (None, Some(error)) => {
            validate_rpc_error(error)?;
            Ok(Some(McpRpcTerminal::Error(error.clone())))
        }
        _ => anyhow::bail!(
            "MCP response id {expected_id} must contain exactly one of result or error"
        ),
    }
}

fn validate_rpc_error(error: &Value) -> Result<()> {
    let error = error
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("JSON-RPC error must be an object"))?;
    if error.get("code").and_then(Value::as_i64).is_none()
        || error.get("message").and_then(Value::as_str).is_none()
    {
        anyhow::bail!("JSON-RPC error must contain an integer code and string message");
    }
    Ok(())
}

fn rpc_error_message(error: &Value) -> String {
    let code = error.get("code").and_then(Value::as_i64);
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("unspecified JSON-RPC error");
    code.map_or_else(|| message.to_string(), |code| format!("{code}: {message}"))
}

fn mcp_request_timeout() -> Duration {
    let timeout_s = resolve_settings()
        .ok()
        .and_then(|settings| upstream_setting_i64(&settings, "mcpRequestTimeoutSeconds"))
        .unwrap_or(30)
        .clamp(1, MAX_MCP_REQUEST_TIMEOUT_SECONDS);
    Duration::from_secs(u64::try_from(timeout_s).unwrap_or(1))
}

fn mcp_request_deadline() -> Result<Instant> {
    Instant::now()
        .checked_add(mcp_request_timeout())
        .ok_or_else(|| anyhow::anyhow!("MCP request deadline overflowed"))
}

fn mcp_enabled() -> Result<bool> {
    let settings = resolve_settings()?;
    Ok(settings
        .upstream_settings
        .get("mcpNativeEnabled")
        .and_then(Value::as_bool)
        .unwrap_or(false))
}

#[derive(Debug, PartialEq, Eq)]
struct McpExecutableIdentity {
    canonical_path: PathBuf,
    bytes: u64,
    sha256: String,
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    device: u64,
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    inode: u64,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
struct PendingManagedMcpFile {
    path: PathBuf,
    armed: bool,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl PendingManagedMcpFile {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl Drop for PendingManagedMcpFile {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
struct ManagedMcpWriteGuard {
    lock: File,
    dir: PathBuf,
    sealed: bool,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl ManagedMcpWriteGuard {
    fn seal(&mut self) -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&self.dir, fs::Permissions::from_mode(0o500))?;
        File::open(&self.dir)?.sync_all()?;
        self.sealed = true;
        Ok(())
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl Drop for ManagedMcpWriteGuard {
    fn drop(&mut self) {
        if !self.sealed {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&self.dir, fs::Permissions::from_mode(0o500));
            let _ = File::open(&self.dir).and_then(|dir| dir.sync_all());
        }
        let _ = FileExt::unlock(&self.lock);
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
struct ManagedMcpReadGuard {
    lock: File,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl Drop for ManagedMcpReadGuard {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.lock);
    }
}

pub(crate) fn exact_mcp_server_config_sha256(config: &McpServerConfig) -> Result<String> {
    let executable = mcp_executable_identity(&config.command)?;
    mcp_server_config_sha256_with_identity(config, &executable)
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
fn freeze_mcp_server_for_persona(
    config: &McpServerConfig,
    data_dir: &Path,
    expected_config_sha256: &str,
) -> Result<(McpServerConfig, String)> {
    let mut managed_store = lock_managed_mcp_store_for_write(data_dir)?;
    let (frozen, frozen_hash, _newly_staged) =
        stage_mcp_server_for_persona_locked(config, expected_config_sha256, &mut managed_store)?;
    managed_store.seal()?;
    drop(managed_store);
    validate_frozen_mcp_server(&frozen, data_dir, &frozen_hash)?;
    Ok((frozen, frozen_hash))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn stage_mcp_server_for_persona_locked(
    config: &McpServerConfig,
    expected_config_sha256: &str,
    managed_store: &mut ManagedMcpWriteGuard,
) -> Result<(McpServerConfig, String, bool)> {
    if !config.enabled {
        anyhow::bail!("Persona MCP server is disabled");
    }
    if !config.args.is_empty() {
        anyhow::bail!(
            "Persona MCP approvals require a reviewed direct native executable with no arguments"
        );
    }
    let source_identity = mcp_executable_identity(&config.command)?;
    let current_hash = mcp_server_config_sha256_with_identity(config, &source_identity)?;
    if current_hash != expected_config_sha256 {
        anyhow::bail!("MCP executable identity drifted before reviewed Persona staging");
    }

    let managed_dir = managed_store.dir.clone();
    cleanup_stale_managed_mcp_files(&managed_dir)?;
    validate_managed_mcp_budget(&managed_dir, &source_identity.sha256, source_identity.bytes)?;
    let managed_path = managed_dir.join(format!("{}.bin", source_identity.sha256));
    let mut newly_staged = false;
    if !managed_path.exists() {
        let temp_path = managed_dir.join(format!(
            ".{}.{}.tmp",
            source_identity.sha256,
            Uuid::new_v4()
        ));
        let mut cleanup = PendingManagedMcpFile::new(temp_path.clone());
        let mut target = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)?;
        copy_exact_mcp_executable(&source_identity, &mut target)?;
        target.sync_all()?;
        use std::os::unix::fs::PermissionsExt;
        target.set_permissions(fs::Permissions::from_mode(0o500))?;
        drop(target);
        // Publish only the fully synced private temp inode. This prevents one
        // cooperating Mom writer from replacing another writer's final path;
        // it does not claim atomic pathname-to-exec binding.
        match fs::hard_link(&temp_path, &managed_path) {
            Ok(()) => {
                newly_staged = true;
                fs::remove_file(&temp_path)?;
                cleanup.disarm();
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                fs::remove_file(&temp_path)?;
                cleanup.disarm();
            }
            Err(error) => return Err(error.into()),
        }
        File::open(&managed_dir)?.sync_all()?;
    }

    let frozen = McpServerConfig {
        name: config.name.clone(),
        command: managed_path,
        executable_sha256: Some(source_identity.sha256.clone()),
        args: Vec::new(),
        enabled: true,
    };
    let frozen_hash = exact_mcp_server_config_sha256(&frozen)?;

    // Rechecking the configured pathname detects drift around the reviewed
    // copy. It does not assert an fd-bound exec or identity of dynamic inputs.
    if exact_mcp_server_config_sha256(config)? != expected_config_sha256 {
        anyhow::bail!("MCP executable identity drifted while reviewed staging completed");
    }
    Ok((frozen, frozen_hash, newly_staged))
}

pub(crate) fn validate_frozen_mcp_server(
    config: &McpServerConfig,
    data_dir: &Path,
    expected_config_sha256: &str,
) -> Result<()> {
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (config, data_dir, expected_config_sha256);
        anyhow::bail!("reviewed Persona MCP executable staging is unsupported on this platform");
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        if !config.enabled || !config.args.is_empty() {
            anyhow::bail!(
                "reviewed Persona MCP configuration is not a direct no-argument executable"
            );
        }
        let managed_dir = managed_mcp_executable_dir(data_dir)?;
        let command_parent = config
            .command
            .parent()
            .ok_or_else(|| anyhow::anyhow!("frozen Persona MCP command has no parent"))?
            .canonicalize()?;
        if command_parent != managed_dir {
            anyhow::bail!("frozen Persona MCP command escaped managed storage");
        }
        let _managed_store = managed_mcp_read_guard(&config.command)?.ok_or_else(|| {
            anyhow::anyhow!("frozen Persona MCP command is not in managed storage")
        })?;
        let identity = mcp_executable_identity(&config.command)?;
        let expected_name = format!("{}.bin", identity.sha256);
        if config.command.file_name().and_then(|name| name.to_str()) != Some(&expected_name) {
            anyhow::bail!("frozen Persona MCP command name does not match its content hash");
        }
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::symlink_metadata(&config.command)?.permissions().mode();
        if mode & 0o222 != 0 || mode & 0o111 == 0 {
            anyhow::bail!(
                "reviewed Persona MCP command is not read-only and executable at validation"
            );
        }
        let actual_hash = mcp_server_config_sha256_with_identity(config, &identity)?;
        if actual_hash != expected_config_sha256 {
            anyhow::bail!("frozen Persona MCP executable identity changed");
        }
        Ok(())
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn open_managed_mcp_lock(data_dir: &Path, create: bool) -> Result<File> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let canonical_data_dir = data_dir.canonicalize()?;
    let lock_path = canonical_data_dir.join(PERSONA_MCP_EXECUTABLES_LOCK);
    if let Ok(metadata) = fs::symlink_metadata(&lock_path)
        && (metadata.file_type().is_symlink() || !metadata.is_file())
    {
        anyhow::bail!("managed Persona MCP lock is not a regular file");
    }
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(create)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(&lock_path)?;
    if !lock.metadata()?.is_file() {
        anyhow::bail!("managed Persona MCP lock is not a regular file");
    }
    lock.set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(lock)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn lock_managed_mcp_file(lock: &File, exclusive: bool) -> Result<()> {
    let deadline = Instant::now()
        .checked_add(MANAGED_MCP_LOCK_TIMEOUT)
        .ok_or_else(|| anyhow::anyhow!("managed MCP lock deadline overflowed"))?;
    loop {
        let result = if exclusive {
            FileExt::try_lock_exclusive(lock)
        } else {
            FileExt::try_lock_shared(lock)
        };
        match result {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                let now = Instant::now();
                if now >= deadline {
                    anyhow::bail!("managed Persona MCP storage lock timed out");
                }
                std::thread::sleep(
                    deadline
                        .saturating_duration_since(now)
                        .min(Duration::from_millis(5)),
                );
            }
            Err(error) => return Err(error.into()),
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn lock_managed_mcp_store_for_write(data_dir: &Path) -> Result<ManagedMcpWriteGuard> {
    use std::os::unix::fs::PermissionsExt;

    let lock = open_managed_mcp_lock(data_dir, true)?;
    lock_managed_mcp_file(&lock, true)?;
    let canonical_data_dir = data_dir.canonicalize()?;
    let managed_dir = canonical_data_dir.join(PERSONA_MCP_EXECUTABLES_DIR);
    match fs::symlink_metadata(&managed_dir) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            anyhow::bail!("managed Persona MCP executable path is not a private directory")
        }
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => fs::create_dir(&managed_dir)?,
        Err(error) => return Err(error.into()),
    }
    fs::set_permissions(&managed_dir, fs::Permissions::from_mode(0o700))?;
    let canonical_managed_dir = managed_dir.canonicalize()?;
    let guard = ManagedMcpWriteGuard {
        lock,
        dir: canonical_managed_dir,
        sealed: false,
    };
    if guard.dir.parent() != Some(canonical_data_dir.as_path()) {
        anyhow::bail!("managed Persona MCP executable directory escaped product storage");
    }
    Ok(guard)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn managed_mcp_read_guard(command: &Path) -> Result<Option<ManagedMcpReadGuard>> {
    let Some(parent) = command.parent() else {
        return Ok(None);
    };
    if parent.file_name().and_then(|name| name.to_str()) != Some(PERSONA_MCP_EXECUTABLES_DIR) {
        return Ok(None);
    }
    let data_dir = parent
        .parent()
        .ok_or_else(|| anyhow::anyhow!("managed MCP command has no product data parent"))?;
    let expected_parent = data_dir.canonicalize()?.join(PERSONA_MCP_EXECUTABLES_DIR);
    let metadata = fs::symlink_metadata(parent)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        anyhow::bail!("managed MCP command parent is not a regular directory");
    }
    if parent.canonicalize()? != expected_parent {
        anyhow::bail!("managed MCP command escaped product storage");
    }
    let lock = open_managed_mcp_lock(data_dir, false)?;
    lock_managed_mcp_file(&lock, false)?;
    use std::os::unix::fs::PermissionsExt;
    if fs::symlink_metadata(parent)?.permissions().mode() & 0o222 != 0 {
        anyhow::bail!("managed MCP command directory is writable during admission");
    }
    Ok(Some(ManagedMcpReadGuard { lock }))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn cleanup_stale_managed_mcp_files(managed_dir: &Path) -> Result<()> {
    let mut visited = 0usize;
    for entry in fs::read_dir(managed_dir)? {
        visited = visited
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("managed MCP directory count overflowed"))?;
        if visited > MAX_MANAGED_MCP_DIRECTORY_ENTRIES {
            anyhow::bail!("managed Persona MCP storage has too many directory entries");
        }
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() || !file_type.is_file() {
            anyhow::bail!("managed Persona MCP storage contains an unexpected entry");
        }
        let file_name = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("managed Persona MCP filename is not UTF-8"))?;
        if file_name.ends_with(".bin") {
            continue;
        }
        let parts = file_name.split('.').collect::<Vec<_>>();
        let is_exact_temp = parts.len() == 4
            && parts[0].is_empty()
            && parts[1].len() == 64
            && parts[1].bytes().all(|byte| byte.is_ascii_hexdigit())
            && Uuid::parse_str(parts[2]).is_ok()
            && parts[3] == "tmp";
        if !is_exact_temp {
            anyhow::bail!("managed Persona MCP storage contains an unrecognized file");
        }
        fs::remove_file(entry.path())?;
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn managed_mcp_executable_dir(data_dir: &Path) -> Result<PathBuf> {
    let canonical_data_dir = data_dir.canonicalize()?;
    let managed_dir = canonical_data_dir.join(PERSONA_MCP_EXECUTABLES_DIR);
    let metadata = fs::symlink_metadata(&managed_dir)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        anyhow::bail!("managed Persona MCP executable path is not a private directory");
    }
    let canonical_managed_dir = managed_dir.canonicalize()?;
    if canonical_managed_dir.parent() != Some(canonical_data_dir.as_path()) {
        anyhow::bail!("managed Persona MCP executable directory escaped product storage");
    }
    use std::os::unix::fs::PermissionsExt;
    if metadata.permissions().mode() & 0o222 != 0 {
        anyhow::bail!("managed Persona MCP executable directory is writable");
    }
    Ok(canonical_managed_dir)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn validate_managed_mcp_budget(
    managed_dir: &Path,
    candidate_sha256: &str,
    candidate_bytes: u64,
) -> Result<()> {
    let mut entries = 0usize;
    let mut total_bytes = 0u64;
    let mut candidate_present = false;
    for entry in fs::read_dir(managed_dir)? {
        entries = entries
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("managed MCP entry count overflowed"))?;
        if entries > MAX_PERSONA_MCP_EXECUTABLES {
            anyhow::bail!(
                "managed Persona MCP storage exceeds the {} entry limit",
                MAX_PERSONA_MCP_EXECUTABLES
            );
        }
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() || !file_type.is_file() {
            anyhow::bail!("managed Persona MCP storage contains an unexpected entry");
        }
        let file_name = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("managed Persona MCP filename is not UTF-8"))?;
        let Some(digest) = file_name.strip_suffix(".bin") else {
            anyhow::bail!("managed Persona MCP storage contains an unrecognized file");
        };
        if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            anyhow::bail!("managed Persona MCP filename is not content-addressed");
        }
        let bytes = entry.metadata()?.len();
        total_bytes = total_bytes
            .checked_add(bytes)
            .ok_or_else(|| anyhow::anyhow!("managed Persona MCP byte count overflowed"))?;
        if total_bytes > MAX_PERSONA_MCP_EXECUTABLE_BYTES {
            anyhow::bail!(
                "managed Persona MCP storage exceeds the {} byte limit",
                MAX_PERSONA_MCP_EXECUTABLE_BYTES
            );
        }
        candidate_present |= digest == candidate_sha256;
    }
    if !candidate_present
        && (entries >= MAX_PERSONA_MCP_EXECUTABLES
            || total_bytes
                .checked_add(candidate_bytes)
                .is_none_or(|total| total > MAX_PERSONA_MCP_EXECUTABLE_BYTES))
    {
        anyhow::bail!("managed Persona MCP storage has no capacity for this executable");
    }
    Ok(())
}

fn mcp_executable_identity(command: &Path) -> Result<McpExecutableIdentity> {
    mcp_executable_identity_with_file(command).map(|(identity, _file)| identity)
}

fn mcp_executable_identity_with_file(command: &Path) -> Result<(McpExecutableIdentity, File)> {
    read_mcp_command_identity(command, true)
}

fn read_mcp_command_identity(
    command: &Path,
    require_native: bool,
) -> Result<(McpExecutableIdentity, File)> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    use std::os::unix::fs::MetadataExt;

    if !command.is_absolute() {
        anyhow::bail!("MCP command must be an absolute executable path");
    }
    let path_metadata = fs::symlink_metadata(command)?;
    if require_native && (path_metadata.file_type().is_symlink() || !path_metadata.is_file()) {
        anyhow::bail!("MCP command must be a nonsymlink regular file");
    }
    let canonical_path = command.canonicalize()?;
    let mut executable = File::open(&canonical_path)?;
    let metadata = executable.metadata()?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_MCP_EXECUTABLE_BYTES {
        anyhow::bail!(
            "MCP command size must be between 1 and {} bytes",
            MAX_MCP_EXECUTABLE_BYTES
        );
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            anyhow::bail!("MCP command is not executable");
        }
    }
    let mut digest = Sha256::new();
    let mut magic = [0_u8; 4];
    executable.read_exact(&mut magic)?;
    if require_native && !is_native_executable_magic(magic) {
        anyhow::bail!(
            "Persona MCP approvals require a native executable for the current platform, not a script or interpreter input"
        );
    }
    digest.update(magic);
    let mut buffer = [0_u8; 64 * 1024];
    let mut bytes = 4_u64;
    loop {
        let read = executable.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        bytes = bytes
            .checked_add(u64::try_from(read)?)
            .ok_or_else(|| anyhow::anyhow!("MCP executable byte count overflowed"))?;
        if bytes > MAX_MCP_EXECUTABLE_BYTES {
            anyhow::bail!("MCP command changed beyond its byte limit while hashing");
        }
        digest.update(&buffer[..read]);
    }
    if bytes != metadata.len() {
        anyhow::bail!("MCP command changed size while hashing");
    }
    let current_path_metadata = if require_native {
        fs::symlink_metadata(command)?
    } else {
        fs::metadata(command)?
    };
    if current_path_metadata.file_type().is_symlink() || !current_path_metadata.is_file() {
        anyhow::bail!("MCP command changed type while hashing");
    }
    if command.canonicalize()? != canonical_path {
        anyhow::bail!("MCP command path changed while hashing");
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        if current_path_metadata.dev() != metadata.dev()
            || current_path_metadata.ino() != metadata.ino()
        {
            anyhow::bail!("MCP command inode changed while hashing");
        }
    }
    let identity = McpExecutableIdentity {
        canonical_path,
        bytes,
        sha256: format!("{:x}", digest.finalize()),
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        device: metadata.dev(),
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        inode: metadata.ino(),
    };
    Ok((identity, executable))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn copy_exact_mcp_executable(expected: &McpExecutableIdentity, target: &mut File) -> Result<()> {
    let mut source = File::open(&expected.canonical_path)?;
    if source.metadata()?.len() != expected.bytes {
        anyhow::bail!("MCP command changed before managed copy");
    }
    let mut digest = Sha256::new();
    let mut copied = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = source.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        copied = copied
            .checked_add(u64::try_from(read)?)
            .ok_or_else(|| anyhow::anyhow!("MCP executable copy byte count overflowed"))?;
        if copied > expected.bytes {
            anyhow::bail!("MCP command grew while creating managed copy");
        }
        digest.update(&buffer[..read]);
        target.write_all(&buffer[..read])?;
    }
    if copied != expected.bytes || format!("{:x}", digest.finalize()) != expected.sha256 {
        anyhow::bail!("MCP command changed while creating managed copy");
    }
    Ok(())
}

fn mcp_server_config_sha256_with_identity(
    config: &McpServerConfig,
    executable: &McpExecutableIdentity,
) -> Result<String> {
    let canonical = serde_json::to_vec(&json!({
        "schema": "mom_llama.mcp_server_identity.v2",
        "name": config.name,
        "command": executable.canonical_path,
        "args": config.args,
        "enabled": config.enabled,
        "executable_bytes": executable.bytes,
        "executable_sha256": executable.sha256,
    }))?;
    Ok(format!("{:x}", Sha256::digest(canonical)))
}

#[cfg(target_os = "linux")]
const fn is_native_executable_magic(magic: [u8; 4]) -> bool {
    matches!(magic, [0x7f, b'E', b'L', b'F'])
}

#[cfg(target_os = "macos")]
const fn is_native_executable_magic(magic: [u8; 4]) -> bool {
    matches!(
        magic,
        [0xfe, 0xed, 0xfa, 0xce]
            | [0xce, 0xfa, 0xed, 0xfe]
            | [0xfe, 0xed, 0xfa, 0xcf]
            | [0xcf, 0xfa, 0xed, 0xfe]
            | [0xca, 0xfe, 0xba, 0xbe]
            | [0xbe, 0xba, 0xfe, 0xca]
            | [0xca, 0xfe, 0xba, 0xbf]
            | [0xbf, 0xba, 0xfe, 0xca]
    )
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
const fn is_native_executable_magic(_magic: [u8; 4]) -> bool {
    false
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn validate_mcp_command(command: &Path) -> Option<Blocker> {
    let _ = command;
    mcp_platform_blocker()
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
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
    use std::os::unix::fs::PermissionsExt;
    match fs::metadata(command) {
        Ok(metadata) if metadata.permissions().mode() & 0o111 != 0 => {}
        _ => {
            return Some(Blocker::new(
                "mcp_command_not_executable",
                "MCP server command does not have executable permissions.",
                vec!["Choose a local executable that you have reviewed.".to_string()],
            ));
        }
    }
    None
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn mcp_platform_blocker() -> Option<Blocker> {
    Some(Blocker::new(
        "mcp_platform_unsupported",
        "Joined MCP process supervision is not available on this platform.",
        vec![
            "Use MCP on macOS or Linux after reviewing the configured process authority."
                .to_string(),
        ],
    ))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) const fn mcp_platform_blocker() -> Option<Blocker> {
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

#[cfg(test)]
mod tests {
    use super::{
        MCP_PROTOCOL_VERSION, MCP_TOOL_CATALOG_NAMESPACE, McpCallSupervisionError,
        McpEffectTerminal, McpServerConfig, McpTool, McpToolCatalogDb, McpToolCatalogEntry,
        exact_mcp_server_config_sha256, execute_mcp_effect_request_supervised,
        persist_persona_mcp_tool_catalog_with_hook, validate_frozen_mcp_server,
        write_mcp_effect_message,
    };
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    use super::{
        cache_persona_mcp_tool_catalog_with_persistence, freeze_mcp_server_for_persona,
        persist_persona_mcp_tool_catalog,
    };
    use crate::store::RuntimeStore;
    use anyhow::Result;
    use serde_json::json;
    use sha2::{Digest, Sha256};
    use std::io::{Error, ErrorKind, Write};
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::thread;

    struct PartialWriter {
        remaining: usize,
    }

    impl Write for PartialWriter {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            if self.remaining == 0 {
                return Err(Error::new(ErrorKind::BrokenPipe, "injected partial write"));
            }
            let written = buffer.len().min(self.remaining);
            self.remaining -= written;
            Ok(written)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            if self.remaining == 0 {
                Err(Error::new(ErrorKind::BrokenPipe, "injected flush failure"))
            } else {
                Ok(())
            }
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn replacing_a_configured_mcp_file_cannot_spawn_or_claim_an_unknown_effect() {
        use std::os::unix::fs::PermissionsExt;
        let directory =
            std::env::temp_dir().join(format!("mom-mcp-drift-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).expect("fixture directory");
        let command = directory.join("server");
        std::fs::write(&command, b"#!/bin/sh\nexit 0\n").expect("fixture script");
        std::fs::set_permissions(&command, std::fs::Permissions::from_mode(0o700))
            .expect("fixture permissions");
        let identity = super::read_mcp_command_identity(&command, false)
            .expect("configured identity")
            .0;
        let mut server = McpServerConfig {
            executable_sha256: Some(identity.sha256),
            name: "drift".into(),
            command: command.clone(),
            args: Vec::new(),
            enabled: true,
        };
        std::fs::write(&command, b"#!/bin/sh\ntouch executed\n").expect("replace executable");
        let error =
            execute_mcp_effect_request_supervised(&server, "lookup", json!({}), "unused", &|| {
                false
            })
            .expect_err("changed executable");
        assert!(!error.outcome_unknown());
        assert!(
            error
                .to_string()
                .contains("differs from its saved configuration")
        );
        assert!(!directory.join("executed").exists());
        server.executable_sha256 = None;
        assert!(super::McpStdioSession::spawn(&server).is_err());
        assert!(!directory.join("executed").exists());
        std::fs::remove_dir_all(directory).expect("fixture cleanup");
    }

    #[test]
    fn cancellation_before_spawn_is_proven_before_effect() {
        let server = McpServerConfig {
            executable_sha256: None,
            name: "missing".to_string(),
            command: PathBuf::from("/definitely/not/a/real/mcp/server"),
            args: Vec::new(),
            enabled: true,
        };
        let error =
            execute_mcp_effect_request_supervised(&server, "lookup", json!({}), "unused", &|| true)
                .expect_err("pre-cancelled call must not spawn");
        assert!(!error.outcome_unknown());
        assert!(error.to_string().contains("before server spawn"));
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn failed_os_spawn_remains_proven_before_external_process_authority() {
        use std::os::unix::fs::PermissionsExt;
        let directory =
            std::env::temp_dir().join(format!("mom-mcp-exec-failure-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).expect("fixture directory");
        let command = directory.join("server");
        std::fs::write(&command, b"#!/definitely/not/a/real/interpreter\n")
            .expect("fixture script");
        std::fs::set_permissions(&command, std::fs::Permissions::from_mode(0o700))
            .expect("fixture permissions");
        let identity = super::read_mcp_command_identity(&command, false)
            .expect("fixture identity")
            .0;
        let server = McpServerConfig {
            executable_sha256: Some(identity.sha256),
            name: "missing-interpreter".to_string(),
            command,
            args: Vec::new(),
            enabled: true,
        };
        let error =
            execute_mcp_effect_request_supervised(&server, "lookup", json!({}), "unused", &|| {
                false
            })
            .expect_err("failed OS spawn must not cross the external-process boundary");
        assert!(!error.outcome_unknown());
        assert!(error.to_string().contains("failed to start MCP server"));
        std::fs::remove_dir_all(directory).expect("fixture cleanup");
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn successful_spawn_makes_initialize_failure_outcome_unknown() {
        let server = McpServerConfig {
            executable_sha256: Some(
                super::read_mcp_command_identity(std::path::Path::new("/bin/sh"), false)
                    .expect("shell identity")
                    .0
                    .sha256,
            ),
            name: "exits-during-initialize".to_string(),
            command: PathBuf::from("/bin/sh"),
            args: vec!["-c".to_string(), "exit 0".to_string()],
            enabled: true,
        };
        let error =
            execute_mcp_effect_request_supervised(&server, "lookup", json!({}), "unused", &|| {
                false
            })
            .expect_err("initialize failure after spawn must be outcome-unknown");
        assert!(error.outcome_unknown());
    }

    #[test]
    fn post_spawn_path_identity_drift_is_outcome_unknown() {
        let error = McpCallSupervisionError::at_process_spawn_boundary(
            "managed MCP pathname identity drifted across process spawn",
            true,
        );
        assert!(error.outcome_unknown());
    }

    #[test]
    fn partial_effect_frame_write_is_outcome_unknown() {
        let error = write_mcp_effect_message(
            &mut PartialWriter { remaining: 1 },
            json!({"name": "lookup", "arguments": {"query": "exact"}}),
        )
        .expect_err("partial effect frame must fail");
        assert!(error.outcome_unknown());
    }

    #[test]
    fn effect_frame_failure_before_first_byte_is_proven_before_effect() {
        let error = write_mcp_effect_message(
            &mut PartialWriter { remaining: 0 },
            json!({"name": "lookup", "arguments": {"query": "exact"}}),
        )
        .expect_err("zero-byte effect frame must fail");
        assert!(!error.outcome_unknown());
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn schema_drift_after_spawn_is_outcome_unknown() {
        let server = standard_shell_server(
            "printf '{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"content\":[]}}\\n'",
        );
        let error = execute_mcp_effect_request_supervised(
            &server,
            "lookup",
            json!({"query": "exact"}),
            "wrong-schema-hash",
            &|| false,
        )
        .expect_err("schema drift is discovered only after process spawn");
        assert!(error.outcome_unknown());
        assert!(error.to_string().contains("schema changed"));
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn malformed_reply_after_effect_dispatch_is_outcome_unknown() {
        let server = standard_shell_server("printf 'malformed-response\\n'");
        let error = execute_mcp_effect_request_supervised(
            &server,
            "lookup",
            json!({"query": "exact"}),
            &object_schema_sha256(),
            &|| false,
        )
        .expect_err("malformed terminal reply must fail closed");
        assert!(error.outcome_unknown());
        assert!(error.to_string().contains("not one JSON line"));
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn wrong_response_id_after_effect_dispatch_is_outcome_unknown() {
        let server =
            standard_shell_server("printf '{\"jsonrpc\":\"2.0\",\"id\":99,\"result\":{}}\\n'");
        let error = execute_mcp_effect_request_supervised(
            &server,
            "lookup",
            json!({"query": "exact"}),
            &object_schema_sha256(),
            &|| false,
        )
        .expect_err("wrong response id must fail closed");
        assert!(error.outcome_unknown());
        assert!(error.to_string().contains("exact response id 3"));
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn exact_json_rpc_error_is_a_known_terminal_rejection() {
        let server = standard_shell_server(
            "printf '{\"jsonrpc\":\"2.0\",\"id\":3,\"error\":{\"code\":-32001,\"message\":\"denied\"}}\\n'",
        );
        let terminal = execute_mcp_effect_request_supervised(
            &server,
            "lookup",
            json!({"query": "exact"}),
            &object_schema_sha256(),
            &|| false,
        )
        .expect("exact error response is a known terminal");
        assert!(matches!(terminal, McpEffectTerminal::Rejected(_)));
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn mcp_is_error_result_is_a_known_terminal_rejection() {
        let server = standard_shell_server(
            "printf '{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"denied\"}],\"isError\":true}}\\n'",
        );
        let terminal = execute_mcp_effect_request_supervised(
            &server,
            "lookup",
            json!({"query": "exact"}),
            &object_schema_sha256(),
            &|| false,
        )
        .expect("isError is a known MCP terminal");
        assert!(matches!(terminal, McpEffectTerminal::Rejected(_)));
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn malformed_call_result_after_dispatch_is_outcome_unknown() {
        let server = standard_shell_server(
            "printf '{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"content\":\"wrong\"}}\\n'",
        );
        let error = execute_mcp_effect_request_supervised(
            &server,
            "lookup",
            json!({"query": "exact"}),
            &object_schema_sha256(),
            &|| false,
        )
        .expect_err("malformed CallToolResult must fail closed");
        assert!(error.outcome_unknown());
        assert!(error.to_string().contains("content array"));
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn malformed_json_rpc_error_after_dispatch_is_outcome_unknown() {
        let server = standard_shell_server(
            "printf '{\"jsonrpc\":\"2.0\",\"id\":3,\"error\":{\"message\":4}}\\n'",
        );
        let error = execute_mcp_effect_request_supervised(
            &server,
            "lookup",
            json!({"query": "exact"}),
            &object_schema_sha256(),
            &|| false,
        )
        .expect_err("malformed JSON-RPC error must fail closed");
        assert!(error.outcome_unknown());
        assert!(
            error
                .to_string()
                .contains("integer code and string message")
        );
    }

    #[test]
    fn reviewed_catalog_updates_serialize_one_mutable_fact() -> Result<()> {
        let data_dir = std::env::temp_dir().join(format!(
            "mom-llama-mcp-catalog-race-{}",
            uuid::Uuid::new_v4()
        ));
        let store = RuntimeStore::open_with_key(&data_dir, [61_u8; 32])?;
        let first_store = store.clone();
        let (first_entered_tx, first_entered_rx) = mpsc::channel();
        let (release_first_tx, release_first_rx) = mpsc::channel();
        let first = thread::spawn(move || {
            persist_persona_mcp_tool_catalog_with_hook(&first_store, catalog_entry("alpha"), |_| {
                first_entered_tx.send(()).expect("signal first transaction");
                release_first_rx.recv().expect("release first transaction");
                Ok(())
            })
        });
        first_entered_rx.recv()?;

        let second_store = store.clone();
        let (second_started_tx, second_started_rx) = mpsc::channel();
        let (second_entered_tx, second_entered_rx) = mpsc::channel();
        let second = thread::spawn(move || {
            second_started_tx.send(()).expect("signal second contender");
            persist_persona_mcp_tool_catalog_with_hook(
                &second_store,
                catalog_entry("beta"),
                |catalog| {
                    second_entered_tx
                        .send(())
                        .expect("signal second transaction");
                    assert!(
                        catalog.servers.iter().any(|entry| entry.server == "alpha"),
                        "the second immediate transaction must observe the first commit"
                    );
                    Ok(())
                },
            )
        });
        second_started_rx.recv()?;
        assert!(
            matches!(
                second_entered_rx.recv_timeout(std::time::Duration::from_millis(100)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ),
            "a second writer must not enter while the first immediate transaction is live"
        );
        release_first_tx.send(())?;
        first.join().expect("first catalog writer")?;
        second.join().expect("second catalog writer")?;
        second_entered_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("second transaction entered after the first commit");

        let catalog = store
            .get::<McpToolCatalogDb>(MCP_TOOL_CATALOG_NAMESPACE)?
            .expect("reviewed catalog");
        assert_eq!(
            catalog
                .servers
                .iter()
                .map(|entry| entry.server.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "beta"]
        );
        std::fs::remove_dir_all(&data_dir)?;
        Ok(())
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn catalog_fault_rolls_back_only_newly_staged_unreferenced_bytes() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let data_dir = std::env::temp_dir().join(format!(
            "mom-llama-mcp-catalog-fault-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&data_dir)?;
        let store = RuntimeStore::open_with_key(&data_dir, [62_u8; 32])?;
        let server = McpServerConfig {
            executable_sha256: None,
            name: "reviewed".to_string(),
            command: std::env::current_exe()?,
            args: Vec::new(),
            enabled: true,
        };
        let server_hash = exact_mcp_server_config_sha256(&server)?;
        let tools = vec![McpTool {
            name: "lookup".to_string(),
            description: None,
            input_schema: json!({"type": "object"}),
        }];
        let injected = cache_persona_mcp_tool_catalog_with_persistence(
            &store,
            &data_dir,
            &server,
            &tools,
            &server_hash,
            |_, _| anyhow::bail!("injected catalog persistence failure"),
        )
        .expect_err("faulted catalog persistence must fail");
        assert!(
            injected
                .to_string()
                .contains("injected catalog persistence failure")
        );
        let managed_dir = data_dir.join(super::PERSONA_MCP_EXECUTABLES_DIR);
        assert_eq!(std::fs::read_dir(&managed_dir)?.count(), 0);

        let changed = cache_persona_mcp_tool_catalog_with_persistence(
            &store,
            &data_dir,
            &server,
            &tools,
            &server_hash,
            persist_persona_mcp_tool_catalog,
        )?;
        let managed_path = changed[0].clone();
        assert!(managed_path.is_file());

        std::fs::set_permissions(&managed_dir, std::fs::Permissions::from_mode(0o700))?;
        std::fs::remove_file(&managed_path)?;
        std::fs::set_permissions(&managed_dir, std::fs::Permissions::from_mode(0o500))?;
        cache_persona_mcp_tool_catalog_with_persistence(
            &store,
            &data_dir,
            &server,
            &tools,
            &server_hash,
            |_, _| anyhow::bail!("second injected catalog persistence failure"),
        )
        .expect_err("fault after restoring referenced bytes must fail");
        assert!(
            managed_path.is_file(),
            "rollback must preserve newly restored bytes referenced by the durable catalog"
        );

        std::fs::set_permissions(&managed_dir, std::fs::Permissions::from_mode(0o700))?;
        std::fs::remove_dir_all(&data_dir)?;
        Ok(())
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn persona_review_stages_and_revalidates_content_addressed_native_bytes() {
        let data_dir = std::env::temp_dir().join(format!(
            "mom-llama-persona-mcp-freeze-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&data_dir).expect("temporary data directory");
        let configured = McpServerConfig {
            executable_sha256: None,
            name: "exact-native".to_string(),
            command: std::env::current_exe().expect("test executable path"),
            args: Vec::new(),
            enabled: true,
        };
        let configured_hash =
            exact_mcp_server_config_sha256(&configured).expect("configured executable identity");
        let (frozen, frozen_hash) =
            freeze_mcp_server_for_persona(&configured, &data_dir, &configured_hash)
                .expect("managed executable freeze");
        assert_ne!(frozen.command, configured.command);
        assert!(frozen.command.is_file());
        assert!(frozen.args.is_empty());
        assert_eq!(
            frozen.command.parent(),
            Some(
                data_dir
                    .join(super::PERSONA_MCP_EXECUTABLES_DIR)
                    .canonicalize()
                    .expect("managed directory")
                    .as_path()
            )
        );
        validate_frozen_mcp_server(&frozen, &data_dir, &frozen_hash)
            .expect("managed executable revalidation");

        let configured_with_argument = McpServerConfig {
            executable_sha256: None,
            args: vec!["mutable-code-path".to_string()],
            ..configured.clone()
        };
        let with_argument_hash = exact_mcp_server_config_sha256(&configured_with_argument)
            .expect("argument-bearing config identity");
        assert!(
            freeze_mcp_server_for_persona(
                &configured_with_argument,
                &data_dir,
                &with_argument_hash,
            )
            .expect_err("code-bearing arguments must fail closed")
            .to_string()
            .contains("no arguments")
        );

        let script_path = data_dir.join("fixture.sh");
        std::fs::write(&script_path, b"#!/bin/sh\nexit 0\n").expect("script fixture");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o700))
            .expect("script permissions");
        let script_config = McpServerConfig {
            executable_sha256: None,
            name: "script".to_string(),
            command: script_path,
            args: Vec::new(),
            enabled: true,
        };
        assert!(
            exact_mcp_server_config_sha256(&script_config)
                .expect_err("shebang scripts must not enter exact Persona approvals")
                .to_string()
                .contains("native executable for the current platform")
        );

        let symlink_path = data_dir.join("linked-server");
        std::os::unix::fs::symlink(&configured.command, &symlink_path)
            .expect("executable symlink fixture");
        let symlink_config = McpServerConfig {
            executable_sha256: None,
            name: "symlink".to_string(),
            command: symlink_path,
            args: Vec::new(),
            enabled: true,
        };
        assert!(
            exact_mcp_server_config_sha256(&symlink_config)
                .expect_err("symlink executable must fail closed")
                .to_string()
                .contains("nonsymlink regular file")
        );
        std::fs::set_permissions(
            data_dir.join(super::PERSONA_MCP_EXECUTABLES_DIR),
            std::fs::Permissions::from_mode(0o700),
        )
        .expect("managed directory cleanup permissions");
        std::fs::remove_dir_all(&data_dir).expect("temporary data cleanup");
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn standard_shell_server(effect_response: &str) -> McpServerConfig {
        let script = format!(
            "IFS= read -r initialize; \
             printf '{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{{\"protocolVersion\":\"{MCP_PROTOCOL_VERSION}\",\"capabilities\":{{\"tools\":{{}}}},\"serverInfo\":{{\"name\":\"fixture\",\"version\":\"1\"}}}}}}\\n'; \
             IFS= read -r initialized; \
             IFS= read -r list; \
             printf '{{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{{\"tools\":[{{\"name\":\"lookup\",\"inputSchema\":{{\"type\":\"object\"}}}}]}}}}\\n'; \
             IFS= read -r call; \
             {effect_response}"
        );
        McpServerConfig {
            executable_sha256: Some(
                super::read_mcp_command_identity(std::path::Path::new("/bin/sh"), false)
                    .expect("shell identity")
                    .0
                    .sha256,
            ),
            name: "standard".to_string(),
            command: PathBuf::from("/bin/sh"),
            args: vec!["-c".to_string(), script],
            enabled: true,
        }
    }

    fn object_schema_sha256() -> String {
        format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&json!({"type":"object"})).expect("schema"))
        )
    }

    fn catalog_entry(server: &str) -> McpToolCatalogEntry {
        McpToolCatalogEntry {
            server: server.to_string(),
            server_config_sha256: format!("configured-{server}"),
            frozen_server_config: McpServerConfig {
                executable_sha256: None,
                name: server.to_string(),
                command: PathBuf::from(format!("/managed/{server}.bin")),
                args: Vec::new(),
                enabled: true,
            },
            frozen_server_config_sha256: format!("frozen-{server}"),
            tools: vec![McpTool {
                name: "lookup".to_string(),
                description: None,
                input_schema: json!({"type": "object"}),
            }],
        }
    }
}
