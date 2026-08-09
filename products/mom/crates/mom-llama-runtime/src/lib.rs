pub mod attachments;
pub mod chat;
pub mod config;
pub mod consult;
pub mod conversation_store;
pub mod engine;
pub mod kv_cache;
pub mod mcp;
pub mod mentions;
pub mod models;
pub mod native_runtime;
pub mod path_selection;
mod persona_library;
pub mod personas;
pub mod receipts;
pub mod server;
pub mod skill_store;
mod store;
pub mod tool_loop;
pub mod upstream_status;

pub use attachments::{
    AttachmentImportOutput, AttachmentKind, AttachmentPreview, AttachmentRecord, AttachmentState,
    attachment_import, attachment_import_pasted_text, attachment_list, attachment_preview,
};
pub use chat::{
    ChatCancelOutput, ChatRequestState, ChatSendInput, ChatSendOptions, ChatSendOutput,
    ChatSkipReasoningOutput, ChatStreamEvent, chat_cancel, chat_continue, chat_regenerate,
    chat_send, chat_send_stream, chat_skip_reasoning,
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
    ChatTemplatePolicy, Conversation, ConversationBranchSibling, ConversationExecutionProfile,
    ConversationExportFormat, ConversationKind, ConversationMutation, ConversationSearchHit,
    DraftMessage, Message, MessageAttribution, MessageBranchSet, MessageBranchSibling, MessageCopy,
    MessageRole, MessageSpeakerKind, TextAttachmentImport, ToolBinding, conversation_delete,
    conversation_export, conversation_fork, conversation_import_json, conversation_list,
    conversation_new, conversation_rename, conversation_search, conversation_select,
    conversation_siblings, conversation_system_message_update, draft_clear, draft_get,
    draft_update, message_branch_select, message_branches, message_copy, message_delete,
    message_edit, text_attachment_import,
};
pub use engine::{EngineCheckOptions, engine_check, engine_status};
pub use kv_cache::{kv_cache_clear, kv_cache_restore, kv_cache_save, kv_cache_status};
pub use mcp::{
    McpCallToolOutput, McpGetPromptOutput, McpPrompt, McpPromptArgument, McpReadResourceOutput,
    McpResource, McpResourceContent, McpServerConfig, McpStatus, McpTool, mcp_call_tool,
    mcp_configure, mcp_get_prompt, mcp_list_prompts, mcp_list_resources, mcp_list_servers,
    mcp_list_tools, mcp_read_resource, mcp_status,
};
pub use mentions::{
    ChatDispatchOutput, ChatDispatchStreamEvent, MentionCancelOutput, MentionCandidate,
    MentionDispatchInput, MentionInvocation, MentionInvocationState, MentionStreamEvent,
    MentionSynthesisOutput, MentionTargetKind, MentionTargetResult, MentionTargetSnapshot,
    chat_dispatch, chat_dispatch_stream, mention_cancel, mention_candidates, mention_dispatch,
    mention_synthesize,
};
pub use models::{hugging_face_hub_cache_dir, model_list, model_select};
pub use native_runtime::{
    gateway_native_configuration, gateway_native_host_and_model, resident_model_for_profile,
    resident_status, unload_resident_model,
};
pub use path_selection::{PathSelection, PathSelectionKind, path_select};
pub use personas::{
    PersonaFreezeInput, PersonaGroup, PersonaHistoryMode, PersonaUpdateInput, PersonaVersion,
    persona_delete, persona_freeze, persona_get, persona_group_create, persona_group_delete,
    persona_group_list, persona_group_update, persona_instantiate, persona_list, persona_update,
    persona_versions,
};
pub use receipts::{Blocker, CommandReceipt, CommandResult, persist_command_receipt};
pub use server::{
    ModelSlot, ServerConfig, ServerStatus, model_slot_list, model_slot_load, model_slot_unload,
    server_configure, server_start, server_status, server_stop,
};
pub use tool_loop::{
    ActiveToolLoop, ToolLoopApproval, ToolLoopCancelOutput, ToolLoopOutput, ToolLoopRunInput,
    ToolLoopState, ToolLoopStep, ToolLoopStreamEvent, ToolPermission, ToolPermissionPolicy,
    tool_loop_cancel, tool_loop_prepare, tool_loop_run, tool_loop_run_stream, tool_loop_status,
    tool_permission_list, tool_permission_revoke, tool_permission_set,
};

pub const RESULT_SCHEMA: &str = "mom_llama.command_result.v1";
pub const RECEIPT_SCHEMA: &str = "mom_llama.command_receipt.v1";
const GATEWAY_RESPONSE_NAMESPACE_PREFIX: &str = "fte.response.v1:";

pub fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

/// Encrypted product-store adapter for embedded gateway response state.
pub fn gateway_document_get(namespace: &str) -> anyhow::Result<Option<Vec<u8>>> {
    validate_gateway_document_namespace(namespace)?;
    store::RuntimeStore::current()?.get_bytes(namespace)
}

/// Encrypted product-store adapter for embedded gateway response state.
pub fn gateway_document_put(namespace: &str, value: &[u8]) -> anyhow::Result<()> {
    validate_gateway_document_namespace(namespace)?;
    store::RuntimeStore::current()?.put_bytes(namespace, value)
}

/// Encrypted product-store adapter for embedded gateway response state.
pub fn gateway_document_delete(namespace: &str) -> anyhow::Result<bool> {
    validate_gateway_document_namespace(namespace)?;
    store::RuntimeStore::current()?.delete(namespace)
}

fn validate_gateway_document_namespace(namespace: &str) -> anyhow::Result<()> {
    let Some(response_id) = namespace.strip_prefix(GATEWAY_RESPONSE_NAMESPACE_PREFIX) else {
        anyhow::bail!(
            "gateway documents must use the `{GATEWAY_RESPONSE_NAMESPACE_PREFIX}` namespace"
        );
    };
    if response_id.is_empty()
        || response_id.len() > 256
        || !response_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        anyhow::bail!("gateway response IDs must be non-empty safe ASCII identifiers");
    }
    Ok(())
}
