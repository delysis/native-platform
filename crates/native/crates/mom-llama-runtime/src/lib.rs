pub mod attachments;
pub mod chat;
pub mod config;
pub mod consult;
pub mod conversation_store;
pub mod engine;
pub mod kv_cache;
pub mod mcp;
pub mod models;
pub mod native_runtime;
pub mod receipts;
pub mod server;
pub mod skill_store;
mod store;
pub mod tool_loop;
pub mod upstream_status;

pub use attachments::{
    AttachmentImportOutput, AttachmentKind, AttachmentRecord, attachment_import, attachment_list,
};
pub use chat::{
    ChatCancelOutput, ChatRequestState, ChatSendInput, ChatSendOptions, ChatSendOutput,
    ChatStreamEvent, chat_cancel, chat_continue, chat_regenerate, chat_send, chat_send_stream,
};
pub use config::{
    GenerationDefaults, KvCachePolicy, Settings, configure_engine, settings_get, settings_reset,
    settings_update,
};
pub use consult::{
    ConsultCancelOutput, ConsultPanel, ConsultPersona, ConsultRun, ConsultRunState,
    ConsultSeatResult, ConsultStartInput, ConsultStartOptions, ConsultStreamEvent,
    ConsultSynthesis, consult_cancel, consult_panel_create, consult_panel_list, consult_start,
    consult_start_stream, consult_status, consult_synthesize,
};
pub use conversation_store::{
    Conversation, ConversationBranchSibling, ConversationExportFormat, ConversationMutation,
    ConversationSearchHit, DraftMessage, Message, MessageCopy, MessageRole, TextAttachmentImport,
    conversation_delete, conversation_export, conversation_fork, conversation_import_json,
    conversation_list, conversation_new, conversation_rename, conversation_search,
    conversation_select, conversation_siblings, draft_clear, draft_get, draft_update, message_copy,
    message_delete, message_edit, text_attachment_import,
};
pub use engine::{EngineCheckOptions, engine_check};
pub use kv_cache::{kv_cache_clear, kv_cache_restore, kv_cache_save, kv_cache_status};
pub use mcp::{
    McpCallToolOutput, McpGetPromptOutput, McpPrompt, McpPromptArgument, McpReadResourceOutput,
    McpResource, McpResourceContent, McpServerConfig, McpStatus, McpTool, mcp_call_tool,
    mcp_configure, mcp_get_prompt, mcp_list_prompts, mcp_list_resources, mcp_list_servers,
    mcp_list_tools, mcp_read_resource, mcp_status,
};
pub use models::{model_list, model_select};
pub use native_runtime::{resident_status, unload_resident_model};
pub use receipts::{Blocker, CommandReceipt, CommandResult, persist_command_receipt};
pub use server::{
    ModelSlot, ServerConfig, ServerStatus, model_slot_list, model_slot_load, model_slot_unload,
    server_configure, server_start, server_status, server_stop,
};
pub use tool_loop::{ToolLoopOutput, ToolLoopStep, tool_loop_run};

pub const RESULT_SCHEMA: &str = "mom_llama.command_result.v1";
pub const RECEIPT_SCHEMA: &str = "mom_llama.command_receipt.v1";

pub fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}
