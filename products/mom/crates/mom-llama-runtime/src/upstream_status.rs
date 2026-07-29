use crate::receipts::{Blocker, CommandResult};
use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeferredFeatureStatus {
    pub feature: String,
    pub upstream_surface: String,
    pub native_status: String,
    pub reason: String,
}

pub fn mcp_status() -> Result<CommandResult<DeferredFeatureStatus>> {
    Ok(CommandResult::blocked(
        "mom_llama.mcp_status",
        "stub_blocked",
        Blocker::new(
            "mcp_native_disabled",
            "MCP servers are disabled until explicitly enabled in native settings.",
            vec![
                "Run `mom-llama settings update --set mcpNativeEnabled=true --json`.".to_string(),
                "Configure an absolute-path stdio MCP server with `mom-llama mcp configure`."
                    .to_string(),
            ],
        ),
    ))
}
