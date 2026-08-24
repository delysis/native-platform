pub mod attachments;
pub mod chat;
pub mod composer;
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
pub use composer::{
    ComposerAutocompleteAcceptOutput, ComposerAutocompleteAnchor, ComposerAutocompleteCancelOutput,
    ComposerAutocompleteInput, ComposerAutocompleteOutput, composer_autocomplete_accept,
    composer_autocomplete_supervised,
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
    MentionToolApproval, MentionToolApprovalDecision, MentionToolApprovalResolution,
    MentionToolApprovalState, MentionToolEffectOutcome, PersonaToolApprovalRecovery, chat_dispatch,
    chat_dispatch_stream, mention_cancel, mention_candidates, mention_dispatch, mention_synthesize,
    mention_tool_approval_decide, mention_tool_approval_decide_with_recovery,
    mention_tool_approval_list, reconcile_persona_tool_approvals,
    reconcile_persona_tool_approvals_command,
};
pub use models::{hugging_face_hub_cache_dir, model_list, model_select};
pub use native_runtime::{
    ProductShutdownError, resident_model_for_profile, resident_status,
    shutdown_product_runtime_for_process_exit, unload_resident_model,
};
pub use path_selection::{PathSelection, PathSelectionKind, path_select};
pub use personas::{
    PersonaFreezeInput, PersonaGroup, PersonaHistoryMode, PersonaRemovalAttachmentImpact,
    PersonaRemovalCacheImpact, PersonaRemovalCommitInput, PersonaRemovalDraftImpact,
    PersonaRemovalGroupImpact, PersonaRemovalHistoryImpact, PersonaRemovalImpact,
    PersonaRemovalOutput, PersonaUpdateInput, PersonaVersion, persona_freeze, persona_get,
    persona_group_create, persona_group_delete, persona_group_list, persona_group_update,
    persona_instantiate, persona_list, persona_removal_preview, persona_remove_from_library,
    persona_update, persona_versions,
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

/// Requests cancellation from every product operation registry without
/// waiting for completion. The application composition root calls this after
/// closing admission and before it drains services and application leases.
pub fn request_product_cancellation() -> usize {
    mcp::request_all_mcp_cancellation();
    chat::request_all_chat_cancellation()
        .saturating_add(mentions::request_all_mention_cancellation())
        .saturating_add(tool_loop::request_all_tool_loop_cancellation())
}

/// Allows an explicit startup retry to ask the OS credential store again after
/// a denied or cancelled first attempt. Successfully cached installation keys
/// are retained, and encrypted data remains bound to the same store identity.
pub fn prepare_secure_store_retry() -> anyhow::Result<()> {
    store::prepare_secure_store_retry()
}

pub fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}
