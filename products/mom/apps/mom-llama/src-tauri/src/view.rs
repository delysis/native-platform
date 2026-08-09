use anyhow::Result;
use maud::{Markup, PreEscaped, html};
use mom_llama_runtime::{
    AttachmentKind, AttachmentRecord, Blocker, CommandResult, Conversation, ConversationKind,
    DraftMessage, KvCachePolicy, Message, MessageRole, Settings, engine::EngineCheckOutput,
    kv_cache::KvCacheStatus, models::ModelInfo, skill_store::Skill,
};
use serde::Serialize;
use serde_json::Value;
use std::path::Path;

pub struct ControlSpec {
    pub affordance: &'static str,
    pub command: &'static str,
    pub tauri_command: &'static str,
    pub cli: &'static str,
    pub effect: &'static str,
    pub label: &'static str,
}

pub const CONTROL_SPECS: &[ControlSpec] = &[
    ControlSpec {
        affordance: "layout.sidebar_toggle",
        command: "mom_llama.conversation_list",
        tauri_command: "mom_llama_conversation_list",
        cli: "mom-llama conversation list --json",
        effect: "mom_llama.effects.conversation_store.v1",
        label: "Toggle sidebar",
    },
    ControlSpec {
        affordance: "settings.open",
        command: "mom_llama.settings_get",
        tauri_command: "mom_llama_settings_get",
        cli: "mom-llama settings get --json",
        effect: "mom_llama.effects.settings_store.v1",
        label: "Settings",
    },
    ControlSpec {
        affordance: "settings.close",
        command: "mom_llama.settings_get",
        tauri_command: "mom_llama_settings_get",
        cli: "mom-llama settings get --json",
        effect: "mom_llama.effects.settings_store.v1",
        label: "Close settings",
    },
    ControlSpec {
        affordance: "readiness.engine_check",
        command: "mom_llama.engine_check",
        tauri_command: "mom_llama_engine_check",
        cli: "mom-llama engine check --json",
        effect: "mom_llama.effects.engine_check.v1",
        label: "Check",
    },
    ControlSpec {
        affordance: "readiness.engine_configure",
        command: "mom_llama.engine_configure",
        tauri_command: "mom_llama_engine_configure",
        cli: "mom-llama engine configure --model-path <path> --device <auto|cpu|metal> --json",
        effect: "mom_llama.effects.engine_configure.v1",
        label: "Save paths",
    },
    ControlSpec {
        affordance: "model.list",
        command: "mom_llama.model_list",
        tauri_command: "mom_llama_model_list",
        cli: "mom-llama model list --json",
        effect: "mom_llama.effects.model_list.v1",
        label: "Refresh",
    },
    ControlSpec {
        affordance: "readiness.model_select",
        command: "mom_llama.model_select",
        tauri_command: "mom_llama_model_select",
        cli: "mom-llama model select --model-path <path> --json",
        effect: "mom_llama.effects.model_select.v1",
        label: "Use model",
    },
    ControlSpec {
        affordance: "path.select",
        command: "mom_llama.path_select",
        tauri_command: "mom_llama_pick_file",
        cli: "mom-llama path select --kind <model|mmproj|conversation|attachment|mcp> --path <path> --json",
        effect: "mom_llama.effects.path_select.v1",
        label: "Choose file",
    },
    ControlSpec {
        affordance: "chat.composer.send",
        command: "mom_llama.chat_dispatch",
        tauri_command: "mom_llama_chat_dispatch",
        cli: "mom-llama chat dispatch --conversation <id> --message <text> --json",
        effect: "mom_llama.effects.chat_send.v1",
        label: "Send",
    },
    ControlSpec {
        affordance: "mention.candidates",
        command: "mom_llama.mention_candidates",
        tauri_command: "mom_llama_mention_candidates",
        cli: "mom-llama mention candidates --query <text> --json",
        effect: "mom_llama.effects.conversation_store.v1",
        label: "Mention a persona or chat",
    },
    ControlSpec {
        affordance: "mention.cancel",
        command: "mom_llama.mention_cancel",
        tauri_command: "mom_llama_mention_cancel",
        cli: "mom-llama mention cancel --invocation <id> --target <id> --json",
        effect: "mom_llama.effects.chat_cancel.v1",
        label: "Stop invited response",
    },
    ControlSpec {
        affordance: "mention.dispatch",
        command: "mom_llama.mention_dispatch",
        tauri_command: "mom_llama_mention_dispatch",
        cli: "mom-llama mention dispatch --conversation <id> --message <text> --json",
        effect: "mom_llama.effects.chat_send.v1",
        label: "Invite mentioned participants",
    },
    ControlSpec {
        affordance: "persona.freeze",
        command: "mom_llama.persona_freeze",
        tauri_command: "mom_llama_persona_freeze",
        cli: "mom-llama persona freeze --conversation <id> --message <id> --name <name> --handle <handle> --json",
        effect: "mom_llama.effects.conversation_store.v1",
        label: "Freeze as persona",
    },
    ControlSpec {
        affordance: "persona.list",
        command: "mom_llama.persona_list",
        tauri_command: "mom_llama_persona_list",
        cli: "mom-llama persona list --json",
        effect: "mom_llama.effects.conversation_store.v1",
        label: "Personas",
    },
    ControlSpec {
        affordance: "persona.get",
        command: "mom_llama.persona_get",
        tauri_command: "mom_llama_persona_get",
        cli: "mom-llama persona get --persona <id> --json",
        effect: "mom_llama.effects.conversation_store.v1",
        label: "Edit persona",
    },
    ControlSpec {
        affordance: "persona.update",
        command: "mom_llama.persona_update",
        tauri_command: "mom_llama_persona_update",
        cli: "mom-llama persona update --profile <json> --json",
        effect: "mom_llama.effects.conversation_store.v1",
        label: "Save persona",
    },
    ControlSpec {
        affordance: "persona.delete",
        command: "mom_llama.persona_delete",
        tauri_command: "mom_llama_persona_delete",
        cli: "mom-llama persona delete --persona <id> --json",
        effect: "mom_llama.effects.conversation_store.v1",
        label: "Delete persona",
    },
    ControlSpec {
        affordance: "persona.instantiate",
        command: "mom_llama.persona_instantiate",
        tauri_command: "mom_llama_persona_instantiate",
        cli: "mom-llama persona instantiate --persona <id> --json",
        effect: "mom_llama.effects.conversation_store.v1",
        label: "Start chat",
    },
    ControlSpec {
        affordance: "persona_group.list",
        command: "mom_llama.persona_group_list",
        tauri_command: "mom_llama_persona_group_list",
        cli: "mom-llama persona-group list --json",
        effect: "mom_llama.effects.consult_read.v1",
        label: "Consult groups",
    },
    ControlSpec {
        affordance: "persona_group.create",
        command: "mom_llama.persona_group_create",
        tauri_command: "mom_llama_persona_group_create",
        cli: "mom-llama persona-group create --name <name> --handle <handle> --persona <id> --json",
        effect: "mom_llama.effects.consult_store.v1",
        label: "Save consult group",
    },
    ControlSpec {
        affordance: "persona_group.delete",
        command: "mom_llama.persona_group_delete",
        tauri_command: "mom_llama_persona_group_delete",
        cli: "mom-llama persona-group delete --group <id> --json",
        effect: "mom_llama.effects.consult_store.v1",
        label: "Delete consult group",
    },
    ControlSpec {
        affordance: "persona_group.update",
        command: "mom_llama.persona_group_update",
        tauri_command: "mom_llama_persona_group_update",
        cli: "mom-llama persona-group update --group <id> --name <name> --handle <handle> --persona <id> --json",
        effect: "mom_llama.effects.consult_store.v1",
        label: "Update consult group",
    },
    ControlSpec {
        affordance: "chat.composer.cancel",
        command: "mom_llama.chat_cancel",
        tauri_command: "mom_llama_chat_cancel",
        cli: "mom-llama chat cancel --conversation <id> --json",
        effect: "mom_llama.effects.chat_cancel.v1",
        label: "Stop",
    },
    ControlSpec {
        affordance: "chat.composer.skip_reasoning",
        command: "mom_llama.chat_skip_reasoning",
        tauri_command: "mom_llama_chat_skip_reasoning",
        cli: "mom-llama chat skip-reasoning --conversation <id> --json",
        effect: "mom_llama.effects.chat_cancel.v1",
        label: "Skip reasoning",
    },
    ControlSpec {
        affordance: "consult.open",
        command: "mom_llama.persona_group_list",
        tauri_command: "mom_llama_persona_group_list",
        cli: "mom-llama persona-group list --json",
        effect: "mom_llama.effects.consult_read.v1",
        label: "Consult group",
    },
    ControlSpec {
        affordance: "consult.close",
        command: "mom_llama.conversation_list",
        tauri_command: "mom_llama_conversation_list",
        cli: "mom-llama conversation list --json",
        effect: "mom_llama.effects.conversation_store.v1",
        label: "Back to chat",
    },
    ControlSpec {
        affordance: "mention.synthesize",
        command: "mom_llama.mention_synthesize",
        tauri_command: "mom_llama_mention_synthesize",
        cli: "mom-llama mention synthesize --invocation <id> --json",
        effect: "mom_llama.effects.consult_generate.v1",
        label: "Synthesize",
    },
    ControlSpec {
        affordance: "chat.message.regenerate",
        command: "mom_llama.chat_regenerate",
        tauri_command: "mom_llama_chat_regenerate",
        cli: "mom-llama chat regenerate --conversation <id> --json",
        effect: "mom_llama.effects.chat_send.v1",
        label: "Regenerate",
    },
    ControlSpec {
        affordance: "chat.message.continue",
        command: "mom_llama.chat_continue",
        tauri_command: "mom_llama_chat_continue",
        cli: "mom-llama chat continue --conversation <id> --json",
        effect: "mom_llama.effects.chat_send.v1",
        label: "Continue",
    },
    ControlSpec {
        affordance: "conversation.new",
        command: "mom_llama.conversation_new",
        tauri_command: "mom_llama_conversation_new",
        cli: "mom-llama conversation new --title <title> --json",
        effect: "mom_llama.effects.conversation_store.v1",
        label: "New chat",
    },
    ControlSpec {
        affordance: "conversation.list",
        command: "mom_llama.conversation_list",
        tauri_command: "mom_llama_conversation_list",
        cli: "mom-llama conversation list --json",
        effect: "mom_llama.effects.conversation_store.v1",
        label: "Refresh",
    },
    ControlSpec {
        affordance: "conversation.select",
        command: "mom_llama.conversation_select",
        tauri_command: "mom_llama_conversation_select",
        cli: "mom-llama conversation select --conversation <id> --json",
        effect: "mom_llama.effects.conversation_store.v1",
        label: "Open",
    },
    ControlSpec {
        affordance: "conversation.search",
        command: "mom_llama.conversation_search",
        tauri_command: "mom_llama_conversation_search",
        cli: "mom-llama conversation search --query <text> --json",
        effect: "mom_llama.effects.conversation_store.v1",
        label: "Search",
    },
    ControlSpec {
        affordance: "conversation.search.close",
        command: "mom_llama.conversation_list",
        tauri_command: "mom_llama_conversation_list",
        cli: "mom-llama conversation list --json",
        effect: "mom_llama.effects.conversation_store.v1",
        label: "Close search",
    },
    ControlSpec {
        affordance: "conversation.rename",
        command: "mom_llama.conversation_rename",
        tauri_command: "mom_llama_conversation_rename",
        cli: "mom-llama conversation rename --conversation <id> --title <title> --json",
        effect: "mom_llama.effects.conversation_store.v1",
        label: "Rename",
    },
    ControlSpec {
        affordance: "conversation.delete",
        command: "mom_llama.conversation_delete",
        tauri_command: "mom_llama_conversation_delete",
        cli: "mom-llama conversation delete --conversation <id> --json",
        effect: "mom_llama.effects.conversation_store.v1",
        label: "Delete",
    },
    ControlSpec {
        affordance: "conversation.export",
        command: "mom_llama.conversation_export",
        tauri_command: "mom_llama_conversation_export",
        cli: "mom-llama conversation export --conversation <id> --format json --json",
        effect: "mom_llama.effects.conversation_store.v1",
        label: "Export",
    },
    ControlSpec {
        affordance: "conversation.import",
        command: "mom_llama.conversation_import",
        tauri_command: "mom_llama_conversation_import",
        cli: "mom-llama conversation import --path <path> --json",
        effect: "mom_llama.effects.conversation_store.v1",
        label: "Import",
    },
    ControlSpec {
        affordance: "conversation.fork",
        command: "mom_llama.conversation_fork",
        tauri_command: "mom_llama_conversation_fork",
        cli: "mom-llama conversation fork --conversation <id> --message <id> --json",
        effect: "mom_llama.effects.conversation_store.v1",
        label: "Fork",
    },
    ControlSpec {
        affordance: "conversation.siblings",
        command: "mom_llama.conversation_siblings",
        tauri_command: "mom_llama_conversation_siblings",
        cli: "mom-llama conversation siblings --conversation <id> --json",
        effect: "mom_llama.effects.conversation_store.v1",
        label: "Branches",
    },
    ControlSpec {
        affordance: "message.branch.list",
        command: "mom_llama.message_branches",
        tauri_command: "mom_llama_message_branches",
        cli: "mom-llama message branches --conversation <id> --message <id> --json",
        effect: "mom_llama.effects.conversation_store.v1",
        label: "List message branches",
    },
    ControlSpec {
        affordance: "message.branch.previous",
        command: "mom_llama.message_branch_select",
        tauri_command: "mom_llama_message_branch_select",
        cli: "mom-llama message branch-select --conversation <id> --message <id> --json",
        effect: "mom_llama.effects.conversation_store.v1",
        label: "Previous branch",
    },
    ControlSpec {
        affordance: "message.branch.next",
        command: "mom_llama.message_branch_select",
        tauri_command: "mom_llama_message_branch_select",
        cli: "mom-llama message branch-select --conversation <id> --message <id> --json",
        effect: "mom_llama.effects.conversation_store.v1",
        label: "Next branch",
    },
    ControlSpec {
        affordance: "conversation.draft_get",
        command: "mom_llama.draft_get",
        tauri_command: "mom_llama_draft_get",
        cli: "mom-llama conversation draft-get --conversation <id> --json",
        effect: "mom_llama.effects.conversation_store.v1",
        label: "Load draft",
    },
    ControlSpec {
        affordance: "conversation.draft_update",
        command: "mom_llama.draft_update",
        tauri_command: "mom_llama_draft_update",
        cli: "mom-llama conversation draft-update --conversation <id> --message <text> --json",
        effect: "mom_llama.effects.conversation_store.v1",
        label: "Save draft",
    },
    ControlSpec {
        affordance: "conversation.draft_clear",
        command: "mom_llama.draft_clear",
        tauri_command: "mom_llama_draft_clear",
        cli: "mom-llama conversation draft-clear --conversation <id> --json",
        effect: "mom_llama.effects.conversation_store.v1",
        label: "Clear draft",
    },
    ControlSpec {
        affordance: "message.copy",
        command: "mom_llama.message_copy",
        tauri_command: "mom_llama_message_copy",
        cli: "mom-llama message copy --conversation <id> --message <id> --json",
        effect: "mom_llama.effects.conversation_store.v1",
        label: "Copy",
    },
    ControlSpec {
        affordance: "message.raw_toggle",
        command: "mom_llama.message_copy",
        tauri_command: "mom_llama_message_copy",
        cli: "mom-llama message copy --conversation <id> --message <id> --json",
        effect: "mom_llama.effects.conversation_store.v1",
        label: "Raw",
    },
    ControlSpec {
        affordance: "message.edit",
        command: "mom_llama.message_edit",
        tauri_command: "mom_llama_message_edit",
        cli: "mom-llama message edit --conversation <id> --message <id> --content <text> --json",
        effect: "mom_llama.effects.conversation_store.v1",
        label: "Edit",
    },
    ControlSpec {
        affordance: "message.delete",
        command: "mom_llama.message_delete",
        tauri_command: "mom_llama_message_delete",
        cli: "mom-llama message delete --conversation <id> --message <id> --json",
        effect: "mom_llama.effects.conversation_store.v1",
        label: "Delete",
    },
    ControlSpec {
        affordance: "attachment.import_text",
        command: "mom_llama.attachment_import_text",
        tauri_command: "mom_llama_attachment_import_text",
        cli: "mom-llama attachment import-text --conversation <id> --path <path> --json",
        effect: "mom_llama.effects.attachment_import_text.v1",
        label: "Add",
    },
    ControlSpec {
        affordance: "attachment.import_paste",
        command: "mom_llama.attachment_import_paste",
        tauri_command: "mom_llama_attachment_import_paste",
        cli: "mom-llama attachment import-paste --conversation <id> --text <text> --json",
        effect: "mom_llama.effects.attachment_import_paste.v1",
        label: "Attach long paste",
    },
    ControlSpec {
        affordance: "attachment.preview",
        command: "mom_llama.attachment_preview",
        tauri_command: "mom_llama_attachment_preview",
        cli: "mom-llama attachment preview --attachment <id> --json",
        effect: "mom_llama.effects.attachment_preview.v1",
        label: "Preview attachment",
    },
    ControlSpec {
        affordance: "attachment.import",
        command: "mom_llama.attachment_import",
        tauri_command: "mom_llama_attachment_import",
        cli: "mom-llama attachment import --conversation <id> --path <path> --json",
        effect: "mom_llama.effects.attachment_import.v1",
        label: "Attach file",
    },
    ControlSpec {
        affordance: "attachment.list",
        command: "mom_llama.attachment_list",
        tauri_command: "mom_llama_attachment_list",
        cli: "mom-llama attachment list --conversation <id> --json",
        effect: "mom_llama.effects.attachment_list.v1",
        label: "Attachments",
    },
    ControlSpec {
        affordance: "settings.get",
        command: "mom_llama.settings_get",
        tauri_command: "mom_llama_settings_get",
        cli: "mom-llama settings get --json",
        effect: "mom_llama.effects.settings_store.v1",
        label: "Reload",
    },
    ControlSpec {
        affordance: "settings.section",
        command: "mom_llama.settings_get",
        tauri_command: "mom_llama_settings_get",
        cli: "mom-llama settings get --json",
        effect: "mom_llama.effects.settings_store.v1",
        label: "Settings section",
    },
    ControlSpec {
        affordance: "settings.reset",
        command: "mom_llama.settings_reset",
        tauri_command: "mom_llama_settings_reset",
        cli: "mom-llama settings reset --json",
        effect: "mom_llama.effects.settings_store.v1",
        label: "Reset to default",
    },
    ControlSpec {
        affordance: "settings.update",
        command: "mom_llama.settings_update",
        tauri_command: "mom_llama_settings_update",
        cli: "mom-llama settings update --temperature <value> --top-p <value> --max-tokens <n> --json",
        effect: "mom_llama.effects.settings_store.v1",
        label: "Retry saving settings",
    },
    ControlSpec {
        affordance: "conversation.system_message",
        command: "mom_llama.conversation_system_message_update",
        tauri_command: "mom_llama_conversation_system_message_update",
        cli: "mom-llama conversation system-message --conversation <id> --message <text> --json",
        effect: "mom_llama.effects.conversation_store.v1",
        label: "Current chat instructions",
    },
    ControlSpec {
        affordance: "skills.create",
        command: "mom_llama.skill_create",
        tauri_command: "mom_llama_skill_create",
        cli: "mom-llama skill create --name <name> --prompt-template <text> --json",
        effect: "mom_llama.effects.skill_store.v1",
        label: "Save Skill",
    },
    ControlSpec {
        affordance: "skills.list",
        command: "mom_llama.skill_list",
        tauri_command: "mom_llama_skill_list",
        cli: "mom-llama skill list --json",
        effect: "mom_llama.effects.skill_store.v1",
        label: "Refresh Skills",
    },
    ControlSpec {
        affordance: "skills.update",
        command: "mom_llama.skill_update",
        tauri_command: "mom_llama_skill_update",
        cli: "mom-llama skill edit --skill <id> --name <name> --prompt-template <text> --json",
        effect: "mom_llama.effects.skill_store.v1",
        label: "Edit",
    },
    ControlSpec {
        affordance: "skills.apply",
        command: "mom_llama.skill_apply",
        tauri_command: "mom_llama_skill_apply",
        cli: "mom-llama skill apply --conversation <id> --skill <id-or-name> --json",
        effect: "mom_llama.effects.skill_store.v1",
        label: "Use",
    },
    ControlSpec {
        affordance: "kv.status",
        command: "mom_llama.kv_cache_status",
        tauri_command: "mom_llama_kv_cache_status",
        cli: "mom-llama kv-cache status --json",
        effect: "mom_llama.effects.kv_cache_status.v1",
        label: "Cache status",
    },
    ControlSpec {
        affordance: "kv.save",
        command: "mom_llama.kv_cache_save",
        tauri_command: "mom_llama_kv_cache_save",
        cli: "mom-llama kv-cache save --skill <id> --json",
        effect: "mom_llama.effects.kv_cache_mutate.v1",
        label: "Save cache",
    },
    ControlSpec {
        affordance: "kv.restore",
        command: "mom_llama.kv_cache_restore",
        tauri_command: "mom_llama_kv_cache_restore",
        cli: "mom-llama kv-cache restore --cache <id> --json",
        effect: "mom_llama.effects.kv_cache_mutate.v1",
        label: "Restore cache",
    },
    ControlSpec {
        affordance: "kv.clear",
        command: "mom_llama.kv_cache_clear",
        tauri_command: "mom_llama_kv_cache_clear",
        cli: "mom-llama kv-cache clear --json",
        effect: "mom_llama.effects.kv_cache_mutate.v1",
        label: "Clear cache",
    },
    ControlSpec {
        affordance: "mcp.status",
        command: "mom_llama.mcp_status",
        tauri_command: "mom_llama_mcp_status",
        cli: "mom-llama mcp status --json",
        effect: "mom_llama.effects.mcp_config.v1",
        label: "MCP Servers",
    },
    ControlSpec {
        affordance: "mcp.configure",
        command: "mom_llama.mcp_configure",
        tauri_command: "mom_llama_mcp_configure",
        cli: "mom-llama mcp configure --name <name> --command <path> --json",
        effect: "mom_llama.effects.mcp_config.v1",
        label: "Configure MCP",
    },
    ControlSpec {
        affordance: "mcp.list_servers",
        command: "mom_llama.mcp_list_servers",
        tauri_command: "mom_llama_mcp_list_servers",
        cli: "mom-llama mcp list-servers --json",
        effect: "mom_llama.effects.mcp_config.v1",
        label: "MCP servers",
    },
    ControlSpec {
        affordance: "mcp.list_tools",
        command: "mom_llama.mcp_list_tools",
        tauri_command: "mom_llama_mcp_list_tools",
        cli: "mom-llama mcp list-tools --server <name> --json",
        effect: "mom_llama.effects.mcp_stdio.v1",
        label: "List tools",
    },
    ControlSpec {
        affordance: "mcp.call_tool",
        command: "mom_llama.mcp_call_tool",
        tauri_command: "mom_llama_mcp_call_tool",
        cli: "mom-llama mcp call-tool --server <name> --tool <name> --arguments <json> --json",
        effect: "mom_llama.effects.mcp_stdio.v1",
        label: "Call tool",
    },
    ControlSpec {
        affordance: "mcp.list_resources",
        command: "mom_llama.mcp_list_resources",
        tauri_command: "mom_llama_mcp_list_resources",
        cli: "mom-llama mcp list-resources --server <name> --json",
        effect: "mom_llama.effects.mcp_stdio.v1",
        label: "List resources",
    },
    ControlSpec {
        affordance: "mcp.read_resource",
        command: "mom_llama.mcp_read_resource",
        tauri_command: "mom_llama_mcp_read_resource",
        cli: "mom-llama mcp read-resource --server <name> --uri <uri> --json",
        effect: "mom_llama.effects.mcp_stdio.v1",
        label: "Read resource",
    },
    ControlSpec {
        affordance: "mcp.list_prompts",
        command: "mom_llama.mcp_list_prompts",
        tauri_command: "mom_llama_mcp_list_prompts",
        cli: "mom-llama mcp list-prompts --server <name> --json",
        effect: "mom_llama.effects.mcp_stdio.v1",
        label: "List prompts",
    },
    ControlSpec {
        affordance: "mcp.get_prompt",
        command: "mom_llama.mcp_get_prompt",
        tauri_command: "mom_llama_mcp_get_prompt",
        cli: "mom-llama mcp get-prompt --server <name> --prompt <name> --arguments <json> --json",
        effect: "mom_llama.effects.mcp_stdio.v1",
        label: "Get prompt",
    },
    ControlSpec {
        affordance: "tool_loop.prepare",
        command: "mom_llama.tool_loop_prepare",
        tauri_command: "mom_llama_tool_loop_prepare",
        cli: "mom-llama tool-loop prepare --conversation <id> --prompt <text> --server <name> --tool <name> --arguments <json> --json",
        effect: "mom_llama.effects.tool_loop.v1",
        label: "Review tool call",
    },
    ControlSpec {
        affordance: "tool_loop.run",
        command: "mom_llama.tool_loop_run",
        tauri_command: "mom_llama_tool_loop_run",
        cli: "mom-llama tool-loop run --conversation <id> --prompt <text> --server <name> --tool <name> --arguments <json> --approval-id <id> --stream-jsonl",
        effect: "mom_llama.effects.tool_loop.v1",
        label: "Approve once and run",
    },
    ControlSpec {
        affordance: "tool_loop.cancel",
        command: "mom_llama.tool_loop_cancel",
        tauri_command: "mom_llama_tool_loop_cancel",
        cli: "mom-llama tool-loop cancel --conversation <id> --json",
        effect: "mom_llama.effects.tool_loop.v1",
        label: "Stop tool loop",
    },
    ControlSpec {
        affordance: "tool_loop.status",
        command: "mom_llama.tool_loop_status",
        tauri_command: "mom_llama_tool_loop_status",
        cli: "mom-llama tool-loop status --conversation <id> --json",
        effect: "mom_llama.effects.tool_loop.v1",
        label: "Tool loop status",
    },
    ControlSpec {
        affordance: "tool_permission.list",
        command: "mom_llama.tool_permission_list",
        tauri_command: "mom_llama_tool_permission_list",
        cli: "mom-llama tool-loop permission-list --json",
        effect: "mom_llama.effects.tool_permission_store.v1",
        label: "List permissions",
    },
    ControlSpec {
        affordance: "tool_permission.set",
        command: "mom_llama.tool_permission_set",
        tauri_command: "mom_llama_tool_permission_set",
        cli: "mom-llama tool-loop permission-set --server <name> --tool <name> --policy <ask|always-allow|deny> --json",
        effect: "mom_llama.effects.tool_permission_store.v1",
        label: "Save permission",
    },
    ControlSpec {
        affordance: "tool_permission.revoke",
        command: "mom_llama.tool_permission_revoke",
        tauri_command: "mom_llama_tool_permission_revoke",
        cli: "mom-llama tool-loop permission-revoke --server <name> --tool <name> --json",
        effect: "mom_llama.effects.tool_permission_store.v1",
        label: "Revoke permission",
    },
    ControlSpec {
        affordance: "resident.slots",
        command: "mom_llama.model_slot_list",
        tauri_command: "mom_llama_model_slot_list",
        cli: "mom-llama model status --json",
        effect: "mom_llama.effects.model_slot.v1",
        label: "Resident models",
    },
    ControlSpec {
        affordance: "resident.slot_load",
        command: "mom_llama.model_slot_load",
        tauri_command: "mom_llama_model_slot_load",
        cli: "mom-llama model load --slot <n> --model-path <path> --json",
        effect: "mom_llama.effects.model_slot.v1",
        label: "Load model",
    },
    ControlSpec {
        affordance: "resident.slot_unload",
        command: "mom_llama.model_slot_unload",
        tauri_command: "mom_llama_model_slot_unload",
        cli: "mom-llama model unload --slot <n> --json",
        effect: "mom_llama.effects.model_slot.v1",
        label: "Unload model",
    },
];

struct SettingsSectionSpec {
    slug: &'static str,
    title: &'static str,
    icon: &'static str,
    blocker: Option<&'static str>,
}

struct SettingsFieldSpec {
    section: &'static str,
    key: &'static str,
    label: &'static str,
    kind: &'static str,
    help: &'static str,
    options: &'static [(&'static str, &'static str)],
    blocker: Option<&'static str>,
}

const SETTINGS_SECTIONS: &[SettingsSectionSpec] = &[
    SettingsSectionSpec {
        slug: "general",
        title: "General",
        icon: "sliders",
        blocker: None,
    },
    SettingsSectionSpec {
        slug: "display",
        title: "Display",
        icon: "monitor",
        blocker: None,
    },
    SettingsSectionSpec {
        slug: "personas",
        title: "Personas",
        icon: "user-round",
        blocker: None,
    },
    SettingsSectionSpec {
        slug: "consult",
        title: "Consult groups",
        icon: "users",
        blocker: None,
    },
    SettingsSectionSpec {
        slug: "sampling",
        title: "Sampling",
        icon: "funnel",
        blocker: None,
    },
    SettingsSectionSpec {
        slug: "penalties",
        title: "Penalties",
        icon: "alert-triangle",
        blocker: None,
    },
    SettingsSectionSpec {
        slug: "agentic",
        title: "Agentic",
        icon: "list-restart",
        blocker: Some("Tool loops are bounded and run only through configured MCP stdio tools."),
    },
    SettingsSectionSpec {
        slug: "tools",
        title: "Tools",
        icon: "pencil-ruler",
        blocker: None,
    },
    SettingsSectionSpec {
        slug: "mcp",
        title: "MCP",
        icon: "mcp",
        blocker: None,
    },
    SettingsSectionSpec {
        slug: "import-export",
        title: "Import/Export",
        icon: "database",
        blocker: None,
    },
    SettingsSectionSpec {
        slug: "developer",
        title: "Developer",
        icon: "code",
        blocker: None,
    },
];

const THEME_OPTIONS: &[(&str, &str)] =
    &[("system", "System"), ("light", "Light"), ("dark", "Dark")];
const EMPTY_OPTIONS: &[(&str, &str)] = &[];

const SETTINGS_FIELDS: &[SettingsFieldSpec] = &[
    SettingsFieldSpec {
        section: "general",
        key: "theme",
        label: "Theme",
        kind: "select",
        help: "Choose the color theme for the interface.",
        options: THEME_OPTIONS,
        blocker: None,
    },
    SettingsFieldSpec {
        section: "general",
        key: "apiKey",
        label: "API Key",
        kind: "password",
        help: "Saved locally for parity with upstream server API-key mode; native local llama.cpp does not require it.",
        options: EMPTY_OPTIONS,
        blocker: Some("Not used by the native local llama.cpp path."),
    },
    SettingsFieldSpec {
        section: "general",
        key: "systemMessage",
        label: "Default system message",
        kind: "textarea",
        help: "Used by chats that do not have their own instructions.",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
    SettingsFieldSpec {
        section: "general",
        key: "pasteLongTextToFileLen",
        label: "Paste long text to file length",
        kind: "number",
        help: "Convert pasted text at or above this length into an encrypted local attachment. Use 0 to disable.",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
    SettingsFieldSpec {
        section: "general",
        key: "sendOnEnter",
        label: "Send message on Enter",
        kind: "checkbox",
        help: "Use Enter to send messages and Shift + Enter for new lines.",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
    SettingsFieldSpec {
        section: "general",
        key: "copyTextAttachmentsAsPlainText",
        label: "Copy text attachments as plain text",
        kind: "checkbox",
        help: "Copy an attached text message as its plain text payload instead of its attachment wrapper.",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
    SettingsFieldSpec {
        section: "general",
        key: "enableContinueGeneration",
        label: "Enable Continue button",
        kind: "checkbox",
        help: "Show Continue for assistant messages.",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
    SettingsFieldSpec {
        section: "general",
        key: "pdfAsImage",
        label: "Parse PDF as image",
        kind: "checkbox",
        help: "Upstream multimodal preference.",
        options: EMPTY_OPTIONS,
        blocker: Some(
            "PDF files are stored as local attachments; PDF-to-image decoding is not active.",
        ),
    },
    SettingsFieldSpec {
        section: "general",
        key: "titleGenerationUseFirstLine",
        label: "Use first line for title",
        kind: "checkbox",
        help: "Use the first non-empty prompt line for title generation.",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
    SettingsFieldSpec {
        section: "general",
        key: "titleGenerationUseLLM",
        label: "Use LLM for title",
        kind: "checkbox",
        help: "Generate titles from the first exchange.",
        options: EMPTY_OPTIONS,
        blocker: Some(
            "Native title generation uses the first user line; LLM title calls would require an extra explicit local model command.",
        ),
    },
    SettingsFieldSpec {
        section: "general",
        key: "titleGenerationPrompt",
        label: "LLM title prompt",
        kind: "textarea",
        help: "Template for upstream title generation.",
        options: EMPTY_OPTIONS,
        blocker: Some(
            "The prompt is persisted, but automatic LLM title calls are disabled unless a separate local model command is added.",
        ),
    },
    SettingsFieldSpec {
        section: "general",
        key: "maxImageMPixels",
        label: "Maximum image resolution",
        kind: "number",
        help: "Resize images larger than this many megapixels.",
        options: EMPTY_OPTIONS,
        blocker: Some("Image files are stored and gated by mmprojPath; resizing is not active."),
    },
    SettingsFieldSpec {
        section: "display",
        key: "showMessageStats",
        label: "Show message generation statistics",
        kind: "checkbox",
        help: "Display generation statistics below assistant messages.",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
    SettingsFieldSpec {
        section: "display",
        key: "showAgenticTurnStats",
        label: "Show statistics for individual agentic turns",
        kind: "checkbox",
        help: "Display the persisted turn number and native token counts for each tool/model turn.",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
    SettingsFieldSpec {
        section: "display",
        key: "showThoughtInProgress",
        label: "Show thought in progress",
        kind: "checkbox",
        help: "Keep an incomplete streamed reasoning section expanded.",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
    SettingsFieldSpec {
        section: "display",
        key: "alwaysShowToolCallContent",
        label: "Always show tool call content",
        kind: "checkbox",
        help: "Expand tool arguments and technical details in completed local tool cards.",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
    SettingsFieldSpec {
        section: "general",
        key: "autoMicOnEmpty",
        label: "Show microphone on empty input",
        kind: "checkbox",
        help: "Show microphone affordance on empty input.",
        options: EMPTY_OPTIONS,
        blocker: Some(
            "Microphone recording requires a native permissioned capture path; audio files can be attached through Attach file.",
        ),
    },
    SettingsFieldSpec {
        section: "display",
        key: "renderUserContentAsMarkdown",
        label: "Render user content as Markdown",
        kind: "checkbox",
        help: "Render user-authored messages as Markdown.",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
    SettingsFieldSpec {
        section: "display",
        key: "renderThinkingAsMarkdown",
        label: "Render thinking as Markdown",
        kind: "checkbox",
        help: "Render model reasoning as formatted Markdown instead of plain text.",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
    SettingsFieldSpec {
        section: "display",
        key: "fullHeightCodeBlocks",
        label: "Use full height code blocks",
        kind: "checkbox",
        help: "Expand code blocks to full available height.",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
    SettingsFieldSpec {
        section: "display",
        key: "disableAutoScroll",
        label: "Disable automatic scroll",
        kind: "checkbox",
        help: "Disable automatic scrolling during generation.",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
    SettingsFieldSpec {
        section: "display",
        key: "alwaysShowSidebarOnDesktop",
        label: "Always show sidebar on desktop",
        kind: "checkbox",
        help: "Keep sidebar open on desktop layouts.",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
    SettingsFieldSpec {
        section: "display",
        key: "showRawModelNames",
        label: "Show raw model names",
        kind: "checkbox",
        help: "Display full raw model identifiers.",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
    SettingsFieldSpec {
        section: "display",
        key: "showModelQuantization",
        label: "Show model quantization information",
        kind: "checkbox",
        help: "Show the quantization inferred from the selected local GGUF filename.",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
    SettingsFieldSpec {
        section: "display",
        key: "showModelTags",
        label: "Show model tags",
        kind: "checkbox",
        help: "Show native capability tags such as local, multimodal, and reasoning.",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
    SettingsFieldSpec {
        section: "display",
        key: "showBuildVersion",
        label: "Show build version information",
        kind: "checkbox",
        help: "Display the Mom Llama app version in the bottom-right corner.",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
    SettingsFieldSpec {
        section: "display",
        key: "showSystemMessage",
        label: "Show system message",
        kind: "checkbox",
        help: "Display the system message at the top of each conversation.",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
    SettingsFieldSpec {
        section: "sampling",
        key: "temperature",
        label: "Temperature",
        kind: "number",
        help: "Temperature applied by the in-process llama.cpp sampler.",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
    SettingsFieldSpec {
        section: "sampling",
        key: "dynatemp_range",
        label: "Dynamic temperature range",
        kind: "number",
        help: "Dynamic temperature range applied by the in-process sampler.",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
    SettingsFieldSpec {
        section: "sampling",
        key: "dynatemp_exponent",
        label: "Dynamic temperature exponent",
        kind: "number",
        help: "Dynamic temperature exponent applied by the in-process sampler.",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
    SettingsFieldSpec {
        section: "sampling",
        key: "top_k",
        label: "Top K",
        kind: "number",
        help: "Top-K filter applied by the in-process sampler.",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
    SettingsFieldSpec {
        section: "sampling",
        key: "top_p",
        label: "Top P",
        kind: "number",
        help: "Top-P filter applied by the in-process sampler.",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
    SettingsFieldSpec {
        section: "sampling",
        key: "min_p",
        label: "Min P",
        kind: "number",
        help: "Min-P filter applied by the in-process sampler.",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
    SettingsFieldSpec {
        section: "sampling",
        key: "xtc_probability",
        label: "XTC probability",
        kind: "number",
        help: "XTC probability applied by the in-process sampler.",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
    SettingsFieldSpec {
        section: "sampling",
        key: "xtc_threshold",
        label: "XTC threshold",
        kind: "number",
        help: "XTC threshold applied by the in-process sampler.",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
    SettingsFieldSpec {
        section: "sampling",
        key: "typ_p",
        label: "Typical P",
        kind: "number",
        help: "Typical-P filter applied by the in-process sampler.",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
    SettingsFieldSpec {
        section: "sampling",
        key: "max_tokens",
        label: "Max tokens",
        kind: "number",
        help: "Maximum completion tokens decoded by the native model worker.",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
    SettingsFieldSpec {
        section: "sampling",
        key: "samplers",
        label: "Samplers",
        kind: "text",
        help: "Native sampler order, separated by spaces, commas, or semicolons.",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
    SettingsFieldSpec {
        section: "sampling",
        key: "backend_sampling",
        label: "Backend sampling",
        kind: "checkbox",
        help: "Upstream server-side sampling preference.",
        options: EMPTY_OPTIONS,
        blocker: Some("Sampling is always owned by the in-process native model worker."),
    },
    SettingsFieldSpec {
        section: "penalties",
        key: "repeat_last_n",
        label: "Repeat last N",
        kind: "number",
        help: "Token history considered by native repetition penalties.",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
    SettingsFieldSpec {
        section: "penalties",
        key: "repeat_penalty",
        label: "Repeat penalty",
        kind: "number",
        help: "Repetition penalty applied by the in-process sampler.",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
    SettingsFieldSpec {
        section: "penalties",
        key: "presence_penalty",
        label: "Presence penalty",
        kind: "number",
        help: "Presence penalty applied by the in-process sampler.",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
    SettingsFieldSpec {
        section: "penalties",
        key: "frequency_penalty",
        label: "Frequency penalty",
        kind: "number",
        help: "Frequency penalty applied by the in-process sampler.",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
    SettingsFieldSpec {
        section: "penalties",
        key: "dry_multiplier",
        label: "DRY multiplier",
        kind: "number",
        help: "DRY multiplier applied by the in-process sampler.",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
    SettingsFieldSpec {
        section: "penalties",
        key: "dry_base",
        label: "DRY base",
        kind: "number",
        help: "DRY exponential base applied by the in-process sampler.",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
    SettingsFieldSpec {
        section: "penalties",
        key: "dry_allowed_length",
        label: "DRY allowed length",
        kind: "number",
        help: "Allowed repeated sequence length for the native DRY sampler.",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
    SettingsFieldSpec {
        section: "penalties",
        key: "dry_penalty_last_n",
        label: "DRY penalty last N",
        kind: "number",
        help: "Token history considered by the native DRY sampler.",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
    SettingsFieldSpec {
        section: "agentic",
        key: "agenticMaxTurns",
        label: "Agentic turns",
        kind: "number",
        help: "Maximum bounded native model/tool turns (1 to 8).",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
    SettingsFieldSpec {
        section: "mcp",
        key: "mcpRequestTimeoutSeconds",
        label: "Request timeout",
        kind: "number",
        help: "MCP request timeout in seconds.",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
    SettingsFieldSpec {
        section: "mcp",
        key: "mcpServers",
        label: "MCP servers",
        kind: "textarea",
        help: "Upstream MCP server JSON.",
        options: EMPTY_OPTIONS,
        blocker: Some(
            "Executable MCP servers are configured through Configure MCP so absolute paths can be validated.",
        ),
    },
    SettingsFieldSpec {
        section: "developer",
        key: "disableReasoningParsing",
        label: "Disable reasoning parsing",
        kind: "checkbox",
        help: "Keep model reasoning markers in the raw assistant output.",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
    SettingsFieldSpec {
        section: "developer",
        key: "excludeReasoningFromContext",
        label: "Exclude reasoning from context",
        kind: "checkbox",
        help: "Exclude reasoning blocks from future context.",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
    SettingsFieldSpec {
        section: "developer",
        key: "showRawOutputSwitch",
        label: "Enable raw output toggle",
        kind: "checkbox",
        help: "Expose raw output display mode.",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
    SettingsFieldSpec {
        section: "developer",
        key: "jsSandboxEnabled",
        label: "JavaScript sandbox tool",
        kind: "checkbox",
        help: "Upstream browser JavaScript execution authority.",
        options: EMPTY_OPTIONS,
        blocker: Some(
            "Rejected in the local native profile because the webview cannot execute model-authored code.",
        ),
    },
    SettingsFieldSpec {
        section: "developer",
        key: "symbolicMathEnabled",
        label: "Symbolic math",
        kind: "checkbox",
        help: "Upstream nerdamer support inside the JavaScript sandbox.",
        options: EMPTY_OPTIONS,
        blocker: Some(
            "Rejected with the JavaScript sandbox; a bounded native math tool would require its own command contract.",
        ),
    },
    SettingsFieldSpec {
        section: "developer",
        key: "customJson",
        label: "Custom JSON",
        kind: "textarea",
        help: "Allowlisted native sampler overrides encoded as a JSON object.",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
    SettingsFieldSpec {
        section: "developer",
        key: "customCss",
        label: "Custom CSS",
        kind: "textarea",
        help: "Local CSS applied to this native app through a text-only style element.",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
];

const NATIVE_SETTINGS_FIELDS: &[SettingsFieldSpec] = &[
    SettingsFieldSpec {
        section: "agentic",
        key: "agenticMaxToolPreviewLines",
        label: "Max lines per tool preview",
        kind: "number",
        help: "Native-only limit for readable tool result previews.",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
    SettingsFieldSpec {
        section: "mcp",
        key: "mcpNativeEnabled",
        label: "Enable local MCP adapters",
        kind: "checkbox",
        help: "Native-only authority switch for explicitly configured local MCP stdio executables.",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
];

struct StoreProjection<T> {
    value: T,
    blocker: Option<Blocker>,
}

fn store_projection<T>(
    result: Result<CommandResult<T>>,
    unavailable_code: &str,
    unavailable_message: &str,
) -> StoreProjection<T>
where
    T: Default + Serialize,
{
    match result {
        Ok(result) => match result.result {
            Some(value) => StoreProjection {
                value,
                blocker: result.blocker,
            },
            None => StoreProjection {
                value: T::default(),
                blocker: Some(result.blocker.unwrap_or_else(|| {
                    Blocker::new(unavailable_code, unavailable_message, Vec::new())
                })),
            },
        },
        Err(error) => StoreProjection {
            value: T::default(),
            blocker: Some(Blocker::new(
                unavailable_code,
                unavailable_message,
                vec![error.to_string()],
            )),
        },
    }
}

fn store_blocker(blocker: &Blocker) -> Markup {
    html! {
        p class="store-blocker" role="status" data-blocker-code=(blocker.code.clone())
            title=(blocker.next_actions.first().cloned().unwrap_or_default()) {
            (icon_markup("alert-triangle"))
            span { (blocker.message.clone()) }
        }
    }
}

pub fn render_app() -> Result<String> {
    let settings = mom_llama_runtime::settings_get()?;
    let engine = mom_llama_runtime::engine_status()?;
    let conversations = mom_llama_runtime::conversation_list()?;
    let skills = mom_llama_runtime::skill_store::skill_list()?;
    let models = mom_llama_runtime::model_list()?;
    let kv = mom_llama_runtime::kv_cache_status()?;
    let selected_conversation_id =
        mom_llama_runtime::conversation_store::load_db()?.selected_conversation_id;
    let current_conversation_id =
        active_conversation(&conversations, selected_conversation_id.as_deref())
            .map(|conversation| conversation.id)
            .unwrap_or_else(|| "default".to_string());
    let draft = mom_llama_runtime::draft_get(Some(&current_conversation_id))?;
    Ok(app_markup(AppProjection {
        settings: &settings,
        engine: &engine,
        conversations: &conversations,
        skills: &skills,
        models: &models,
        kv: &kv,
        selected_conversation_id: selected_conversation_id.as_deref(),
        draft: &draft,
    })
    .into_string())
}

pub fn render_chat_fragment() -> Result<String> {
    let settings = mom_llama_runtime::settings_get()?;
    let engine = mom_llama_runtime::engine_status()?;
    let conversations = mom_llama_runtime::conversation_list()?;
    let selected = mom_llama_runtime::conversation_store::load_db()?.selected_conversation_id;
    let active = active_conversation(&conversations, selected.as_deref());
    let current_id = active
        .as_ref()
        .map(|conversation| conversation.id.as_str())
        .unwrap_or("default");
    let draft = mom_llama_runtime::draft_get(Some(current_id))?;
    Ok(chat_view_with_draft(&settings, &engine, active.as_ref(), Some(&draft)).into_string())
}

pub fn render_sidebar_fragment() -> Result<String> {
    let conversations = mom_llama_runtime::conversation_list()?;
    let selected = mom_llama_runtime::conversation_store::load_db()?.selected_conversation_id;
    Ok(sidebar(&conversations, selected.as_deref()).into_string())
}

pub fn render_persona_picker_fragment() -> Result<String> {
    Ok(persona_view().into_string())
}

pub fn render_settings_fragment() -> Result<String> {
    let settings = mom_llama_runtime::settings_get()?;
    let engine = mom_llama_runtime::engine_status()?;
    let models = mom_llama_runtime::model_list()?;
    let skills = mom_llama_runtime::skill_store::skill_list()?;
    let kv = mom_llama_runtime::kv_cache_status()?;
    let conversations = mom_llama_runtime::conversation_list()?;
    let selected = mom_llama_runtime::conversation_store::load_db()?.selected_conversation_id;
    let active = active_conversation(&conversations, selected.as_deref());
    Ok(settings_modal(&settings, &engine, &models, &skills, &kv, active.as_ref()).into_string())
}

struct AppProjection<'a> {
    settings: &'a CommandResult<Settings>,
    engine: &'a CommandResult<EngineCheckOutput>,
    conversations: &'a CommandResult<Vec<Conversation>>,
    skills: &'a CommandResult<Vec<Skill>>,
    models: &'a CommandResult<Vec<ModelInfo>>,
    kv: &'a CommandResult<KvCacheStatus>,
    selected_conversation_id: Option<&'a str>,
    draft: &'a CommandResult<DraftMessage>,
}

fn app_markup(projection: AppProjection<'_>) -> Markup {
    let AppProjection {
        settings,
        engine,
        conversations,
        skills,
        models,
        kv,
        selected_conversation_id,
        draft,
    } = projection;
    let active = active_conversation(conversations, selected_conversation_id);
    let theme = upstream_settings_value(settings, "theme");
    let full_height_code = upstream_settings_bool(settings, "fullHeightCodeBlocks");
    let disable_auto_scroll = upstream_settings_bool(settings, "disableAutoScroll");
    let always_show_sidebar = upstream_settings_bool(settings, "alwaysShowSidebarOnDesktop");
    let custom_css = upstream_settings_value(settings, "customCss");
    let show_build_version = upstream_settings_bool(settings, "showBuildVersion");
    let current_conversation_id = active
        .as_ref()
        .map(|conversation| conversation.id.as_str())
        .unwrap_or("default");
    html! {
        div class=(format!(
                "llama-ui-shell{}",
                if full_height_code { " full-height-code" } else { "" }
            ))
            data-theme=(theme)
            data-disable-auto-scroll=(disable_auto_scroll)
            data-always-show-sidebar=(always_show_sidebar)
            data-custom-css=(custom_css)
            data-runtime="tauri-maud-htmx"
            data-native-core-only="true" {
            (sidebar(conversations, active.as_ref().map(|conversation| conversation.id.as_str())))
            header class="chrome" {
                (button("layout.sidebar_toggle", Some("sidebar-toggle"), "icon-button sidebar-toggle", false))
                (button("settings.open", Some("settings-open"), "icon-button settings-toggle", false))
            }
            (chat_view_with_draft(settings, engine, active.as_ref(), Some(draft)))
            (persona_view())
            (consult_view(current_conversation_id))
            (settings_modal(settings, engine, models, skills, kv, active.as_ref()))
            (persona_freeze_modal())
            (tool_approval_modal())
            @if show_build_version {
                small class="build-version" { "Mom Llama " (env!("CARGO_PKG_VERSION")) }
            }
            div id="command-status" class="command-status is-hidden" role="status" aria-live="polite" {}
            output id="command-output" class="sr-command-output" aria-hidden="true" tabindex="-1" {}
        }
    }
}

fn chat_view_with_draft(
    settings: &CommandResult<Settings>,
    engine: &CommandResult<impl Serialize>,
    active: Option<&Conversation>,
    draft: Option<&CommandResult<DraftMessage>>,
) -> Markup {
    let current_id = active
        .map(|conversation| conversation.id.as_str())
        .unwrap_or("default");
    let empty = active
        .map(|conversation| conversation.messages.is_empty())
        .unwrap_or(true);
    let is_persona =
        active.is_some_and(|conversation| conversation.kind == ConversationKind::PersonaTemplate);
    let conversation_kind = if is_persona {
        "persona_template"
    } else {
        "chat"
    };
    let StoreProjection {
        value: attachments,
        blocker: attachment_blocker,
    } = store_projection(
        mom_llama_runtime::attachment_list(Some(current_id)),
        "attachment_store_unavailable",
        "Attachments could not be loaded from local storage.",
    );
    html! {
        main id="chat" class=(format!(
                "chat-main {}{}",
                if empty { "empty" } else { "has-messages" },
                if is_persona { " persona-template" } else { "" },
            ))
            aria-label="Chat interface"
            data-current-conversation=(current_id)
            data-conversation-kind=(conversation_kind) {
            @if let Some(persona) = active.filter(|conversation| {
                conversation.kind == ConversationKind::PersonaTemplate
            }) {
                (persona_context(persona))
            }
            @if let Some(blocker) = &attachment_blocker {
                (store_blocker(blocker))
            }
            @if empty {
                section class="landing" aria-label="Empty chat" {
                    @if let Some(persona) = active.filter(|conversation| {
                        conversation.kind == ConversationKind::PersonaTemplate
                    }) {
                        h1 { (persona.title.clone()) }
                        p { "@" (persona.execution_profile.mention_handle.clone()) }
                    } @else {
                        h1 { "llama.cpp" }
                        p { "Type a message or upload files to get started" }
                    }
                }
            } @else {
                section class="message-stream" aria-label="Messages" {
                    @for message in active.map(|conversation| conversation.messages.as_slice()).unwrap_or(&[]) {
                        @if message.role != MessageRole::System || upstream_settings_bool(settings, "showSystemMessage") {
                            (message_row(
                                message,
                                settings,
                                &message_attachment_records(message, &attachments),
                                !is_persona,
                                active.is_some_and(|conversation| {
                                    conversation.kind == ConversationKind::Chat
                                        && conversation.active_leaf_message_id.as_deref()
                                            == Some(message.id.as_str())
                                        && message.role == MessageRole::Assistant
                                        && message.attribution.is_none()
                                }),
                            ))
                        }
                    }
                    @if let Some(invocation_id) = latest_synthesizable_invocation(active) {
                        button type="button" class="mention-synthesize"
                            data-affordance="mention.synthesize"
                            data-command="mom_llama.mention_synthesize"
                            data-tauri-command="mom_llama_mention_synthesize"
                            data-cli="mom-llama mention synthesize --invocation <id> --json"
                            data-effect="mom_llama.effects.consult_generate.v1"
                            data-action="mention-synthesize"
                            data-invocation=(invocation_id) {
                            (icon_markup("sparkles")) "Synthesize these responses"
                        }
                    }
                }
            }
            (composer(engine, settings, draft, &attachments))
        }
    }
}

fn persona_context(persona: &Conversation) -> Markup {
    let profile = &persona.execution_profile;
    let model = profile
        .model_path
        .as_deref()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .unwrap_or("Default model");
    let profile_control = control("persona.get");
    let start_control = control("persona.instantiate");
    html! {
        header class="persona-context" aria-label="Persona template" {
            span class="persona-context-avatar" { (icon_markup("user-round")) }
            div class="persona-context-copy" {
                strong { (persona.title.clone()) }
                span { "@" (profile.mention_handle.clone()) " · " (model) }
            }
            div class="persona-context-actions" {
                button type="button" class="icon-button"
                    title="Edit profile" aria-label=(format!("Edit {} profile", persona.title))
                    data-affordance=(profile_control.affordance)
                    data-command=(profile_control.command)
                    data-tauri-command=(profile_control.tauri_command)
                    data-cli=(profile_control.cli)
                    data-effect=(profile_control.effect)
                    data-action="persona-profile-open"
                    data-persona=(persona.id.clone()) {
                    (icon_markup("sliders-horizontal"))
                }
                button type="button" class="text-button persona-start-chat"
                    data-affordance=(start_control.affordance)
                    data-command=(start_control.command)
                    data-tauri-command=(start_control.tauri_command)
                    data-cli=(start_control.cli)
                    data-effect=(start_control.effect)
                    data-action="persona-instantiate"
                    data-persona=(persona.id.clone()) {
                    (icon_markup("message-circle")) span { "Start chat" }
                }
            }
        }
    }
}

fn message_attachment_records<'a>(
    message: &Message,
    attachments: &'a [AttachmentRecord],
) -> Vec<&'a AttachmentRecord> {
    if message.attachment_ids.is_empty() {
        return attachments
            .iter()
            .filter(|attachment| attachment.message_id == message.id)
            .collect();
    }
    message
        .attachment_ids
        .iter()
        .filter_map(|id| attachments.iter().find(|attachment| attachment.id == *id))
        .collect()
}

fn latest_synthesizable_invocation(active: Option<&Conversation>) -> Option<String> {
    let mut counts = std::collections::BTreeMap::<String, usize>::new();
    for message in active?.messages.iter().rev() {
        let Some(attribution) = &message.attribution else {
            if !counts.is_empty() {
                break;
            }
            continue;
        };
        if attribution.kind == mom_llama_runtime::MessageSpeakerKind::Synthesis {
            return None;
        }
        *counts.entry(attribution.invocation_id.clone()).or_default() += 1;
    }
    counts
        .into_iter()
        .find_map(|(invocation, count)| (count >= 2).then_some(invocation))
}

fn sidebar(conversations: &CommandResult<Vec<Conversation>>, active_id: Option<&str>) -> Markup {
    html! {
        aside class="sidebar" aria-label="Sidebar" {
            h2 { "llama.cpp" }
            nav class="sidebar-nav" aria-label="Main actions" {
                (button("conversation.new", Some("conversation-new"), "nav-button", false))
                (button("conversation.search", Some("conversation-search-open"), "nav-button", false))
                form id="conversation-search-form" class="nav-search is-hidden"
                    data-affordance="conversation.search"
                    data-command="mom_llama.conversation_search"
                    data-tauri-command="mom_llama_conversation_search"
                    data-cli="mom-llama conversation search --query <text> --json"
                    data-effect="mom_llama.effects.conversation_store.v1" {
                    input name="query" placeholder="Search conversations" aria-label="Search conversations"
                        data-affordance="conversation.search.query"
                        data-command="mom_llama.conversation_search"
                        data-tauri-command="mom_llama_conversation_search"
                        data-cli="mom-llama conversation search --query <text> --json"
                        data-effect="mom_llama.effects.conversation_store.v1";
                    button type="submit"
                        class="icon-button search-submit"
                        data-affordance="conversation.search"
                        data-command="mom_llama.conversation_search"
                        data-tauri-command="mom_llama_conversation_search"
                        data-cli="mom-llama conversation search --query <text> --json"
                        data-effect="mom_llama.effects.conversation_store.v1" {
                        (icon_markup("search")) span class="sr-only" { "Search" }
                    }
                    (button("conversation.search.close", Some("conversation-search-close"), "icon-button search-close", false))
                }
                (button("persona.list", Some("personas-open"), "nav-button", false))
                (button("consult.open", Some("consult-open"), "nav-button", false))
                (button("mcp.status", Some("mcp-status"), "nav-button", false))
            }
            section class="conversation-block" aria-label="Conversations" {
                h3 { "Conversations" }
                ol id="conversation-search-results" class="conversation-list search-results is-hidden" aria-live="polite" {}
                ol id="conversation-list" class="conversation-list" {
                    @if conversations.result.as_deref().unwrap_or(&[]).is_empty() {
                        li class="empty-line" { "No conversations yet" }
                    }
                    @for conversation in conversations.result.as_deref().unwrap_or(&[]).iter()
                        .filter(|conversation| conversation.kind == mom_llama_runtime::ConversationKind::Chat) {
                        @let active = active_id == Some(conversation.id.as_str());
                        li {
                            button type="button"
                                class=(format!("conversation-item {}", if active { "active" } else { "" }))
                                data-affordance="conversation.select"
                                data-command="mom_llama.conversation_select"
                                data-tauri-command="mom_llama_conversation_select"
                                data-cli="mom-llama conversation select --conversation <id> --json"
                                data-effect="mom_llama.effects.conversation_store.v1"
                                data-action="conversation-select"
                                data-conversation=(conversation.id.clone()) {
                                span { (conversation.title.clone()) }
                                small { (message_count(conversation)) }
                            }
                        }
                    }
                }
            }
            div class="sidebar-actions hidden-contract" {
                (button("conversation.rename", Some("conversation-rename"), "small-button", active_id.is_none()))
                (button("conversation.delete", Some("conversation-delete"), "small-button danger", active_id.is_none()))
                (button("conversation.import", Some("conversation-import"), "small-button", false))
            }
        }
    }
}

fn persona_view() -> Markup {
    let StoreProjection {
        value: mut personas,
        blocker,
    } = store_projection(
        mom_llama_runtime::persona_list(),
        "persona_store_unavailable",
        "Saved Personas could not be loaded from local storage.",
    );
    personas.sort_by(|left, right| left.title.cmp(&right.title));
    let settings_open = control("settings.open");
    let select = control("conversation.select");
    let instantiate = control("persona.instantiate");
    html! {
        section id="persona-view" class="persona-picker is-hidden" aria-label="Personas"
            hidden[true] aria-hidden="true" {
            div class="persona-picker-card" role="dialog" aria-modal="true" aria-labelledby="persona-picker-title" {
                header {
                    div {
                        h2 id="persona-picker-title" { "Personas" }
                        p { "Open a saved Persona or start a new chat." }
                    }
                    button type="button" class="icon-button"
                        data-affordance="persona.list"
                        data-command="mom_llama.persona_list"
                        data-tauri-command="mom_llama_persona_list"
                        data-cli="mom-llama persona list --json"
                        data-effect="mom_llama.effects.conversation_store.v1"
                        data-action="personas-close"
                        aria-label="Close Personas" { (icon_markup("x")) }
                }
                div class="persona-picker-options" {
                    @if let Some(blocker) = &blocker {
                        (store_blocker(blocker))
                    } @else if personas.is_empty() {
                        p class="empty-line" { "No Personas have been saved yet." }
                    }
                    @for persona in personas {
                        div class="persona-picker-row" {
                            button type="button" class="persona-picker-option"
                                data-affordance=(select.affordance)
                                data-command=(select.command)
                                data-tauri-command=(select.tauri_command)
                                data-cli=(select.cli)
                                data-effect=(select.effect)
                                data-action="persona-open"
                                data-conversation=(persona.id.clone()) {
                                span class="persona-picker-icon" { (icon_markup("user-round")) }
                                span class="persona-picker-copy" {
                                    strong { (persona.title.clone()) }
                                    small { "@" (persona.execution_profile.mention_handle.clone()) }
                                }
                            }
                            button type="button" class="icon-button persona-picker-launch"
                                title=(format!("Start a chat with {}", persona.title))
                                aria-label=(format!("Start a chat with {}", persona.title))
                                data-affordance=(instantiate.affordance)
                                data-command=(instantiate.command)
                                data-tauri-command=(instantiate.tauri_command)
                                data-cli=(instantiate.cli)
                                data-effect=(instantiate.effect)
                                data-action="persona-instantiate"
                                data-persona=(persona.id.clone()) {
                                (icon_markup("message-circle"))
                            }
                        }
                    }
                }
                button type="button" class="text-button"
                    data-affordance=(settings_open.affordance)
                    data-command=(settings_open.command)
                    data-tauri-command=(settings_open.tauri_command)
                    data-cli=(settings_open.cli)
                    data-effect=(settings_open.effect)
                    data-action="personas-settings-open" { "Manage Personas in Settings" }
            }
        }
    }
}

fn composer(
    engine: &CommandResult<impl Serialize>,
    settings: &CommandResult<Settings>,
    draft: Option<&CommandResult<DraftMessage>>,
    attachments: &[AttachmentRecord],
) -> Markup {
    let enabled = matches!(engine.status.as_str(), "host_integrated" | "configured");
    let draft = draft.and_then(|draft| draft.result.as_ref());
    let draft_message = draft
        .map(|draft| draft.message.as_str())
        .unwrap_or_default();
    let staged_attachments = draft
        .map(|draft| {
            draft
                .attachment_ids
                .iter()
                .filter_map(|id| attachments.iter().find(|attachment| attachment.id == *id))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let draft_control = control("conversation.draft_update");
    html! {
        form id="chat-form" class="composer"
            data-affordance="chat.composer.form"
            data-command="mom_llama.chat_dispatch"
            data-tauri-command="mom_llama_chat_dispatch"
            data-cli="mom-llama chat dispatch --conversation <id> --message <text> --json"
            data-effect="mom_llama.effects.chat_send.v1" {
            @if !staged_attachments.is_empty() {
                div id="composer-attachments" class="composer-attachments"
                    aria-label="Attachments ready to send" {
                    @for attachment in staged_attachments {
                        div class="composer-attachment"
                            data-staged-attachment-id=(attachment.id.clone())
                            data-attachment-kind=(format!("{:?}", attachment.kind).to_lowercase()) {
                            span class="composer-attachment-icon" {
                                (icon_markup(attachment_icon(&attachment.kind)))
                            }
                            span class="composer-attachment-copy" {
                                strong title=(attachment.file_name.clone()) { (attachment.file_name.clone()) }
                                small { (attachment_summary(attachment)) }
                            }
                            button type="button" class="composer-attachment-remove"
                                title=(format!("Remove {}", attachment.file_name))
                                aria-label=(format!("Remove {}", attachment.file_name))
                                data-affordance=(draft_control.affordance)
                                data-command=(draft_control.command)
                                data-tauri-command=(draft_control.tauri_command)
                                data-cli=(draft_control.cli)
                                data-effect=(draft_control.effect)
                                data-action="draft-attachment-remove"
                                data-attachment=(attachment.id.clone()) {
                                (icon_markup("x"))
                            }
                        }
                    }
                }
            }
            textarea name="message"
                rows="2"
                aria-label="Message"
                placeholder="Type a message..."
                data-affordance="chat.composer.message"
                data-command="mom_llama.chat_dispatch"
                data-tauri-command="mom_llama_chat_dispatch"
                data-cli="mom-llama chat dispatch --conversation <id> --message <text> --json"
                data-effect="mom_llama.effects.chat_send.v1"
                data-draft-affordance="conversation.draft_update"
                data-draft-command="mom_llama.draft_update"
                data-draft-tauri-command="mom_llama_draft_update"
                data-draft-cli="mom-llama conversation draft-update --conversation <id> --message <text> --json"
                data-draft-effect="mom_llama.effects.conversation_store.v1"
                data-paste-affordance="attachment.import_paste"
                data-paste-command="mom_llama.attachment_import_paste"
                data-paste-tauri-command="mom_llama_attachment_import_paste"
                data-paste-cli="mom-llama attachment import-paste --conversation <id> --text <text> --json"
                data-paste-effect="mom_llama.effects.attachment_import_paste.v1" {
                (draft_message)
            }
            div id="mention-candidates" class="mention-candidates is-hidden" role="listbox"
                aria-label="Personas, chats, and consult groups"
                data-affordance="mention.candidates"
                data-command="mom_llama.mention_candidates"
                data-tauri-command="mom_llama_mention_candidates"
                data-cli="mom-llama mention candidates --query <text> --json"
                data-effect="mom_llama.effects.conversation_store.v1" {}
            div class="mention-icon-templates" hidden {
                span data-mention-icon="persona" { (icon_markup("user-round")) }
                span data-mention-icon="live_chat" { (icon_markup("message-square")) }
                span data-mention-icon="group" { (icon_markup("users")) }
            }
            div class="composer-bottom" {
                div class="composer-left" {
                    (button("attachment.import", Some("attachment-import"), "round-button", false))
                }
                div class="composer-right" {
                    select name="model_picker" aria-label="Model"
                        data-affordance="readiness.model_select"
                        data-command="mom_llama.model_select"
                        data-tauri-command="mom_llama_model_select"
                        data-cli="mom-llama model select --model-path <path> --json"
                        data-effect="mom_llama.effects.model_select.v1" {
                        @if models_available_for_label(settings) {
                            option value=(model_path(settings)) { (model_chip_label(settings)) }
                        } @else {
                            option value="" { "No model" }
                        }
                    }
                    span class=(format!("runtime-dot {}", if enabled { "ready" } else { "blocked" }))
                        title=(readiness_short_label(engine)) {}
                    (button(
                        "chat.composer.skip_reasoning",
                        Some("chat-skip-reasoning"),
                        "send-button skip-reasoning-button is-hidden",
                        true,
                    ))
                    (button("chat.composer.cancel", Some("chat-cancel"), "send-button stop-button is-hidden", true))
                    button type="submit"
                        class="send-button"
                        data-affordance="chat.composer.send"
                        data-command="mom_llama.chat_dispatch"
                        data-tauri-command="mom_llama_chat_dispatch"
                        data-cli="mom-llama chat dispatch --conversation <id> --message <text> --json"
                        data-effect="mom_llama.effects.chat_send.v1"
                        disabled[!enabled] {
                        "↑"
                    }
                }
            }
            output id="chat-events" class="stream-events" aria-live="polite" {}
        }
        p class="keyboard-hint" { "Press " kbd { "Enter" } " to send, " kbd { "Shift + Enter" } " for new line" }
    }
}

fn consult_view(conversation_id: &str) -> Markup {
    let StoreProjection {
        value: groups,
        blocker,
    } = store_projection(
        mom_llama_runtime::persona_group_list(),
        "persona_group_store_unavailable",
        "Consult groups could not be loaded from local storage.",
    );
    let settings_open = control("settings.open");
    html! {
        section id="consult-view" class="consult-picker is-hidden" aria-label="Consult groups"
            data-current-conversation=(conversation_id) {
            div class="consult-picker-card" {
                header {
                    div { h2 { "Consult groups" } p { "Insert a saved group into this message." } }
                    (button("consult.close", Some("consult-close"), "icon-button", false))
                }
                div class="consult-group-options" {
                    @if let Some(blocker) = &blocker {
                        (store_blocker(blocker))
                    } @else if groups.is_empty() {
                        p class="empty-line" { "No groups yet. Create one in Settings." }
                    }
                    @for group in groups {
                        button type="button" class="consult-group-option"
                            data-affordance="persona_group.list"
                            data-command="mom_llama.persona_group_list"
                            data-tauri-command="mom_llama_persona_group_list"
                            data-cli="mom-llama persona-group list --json"
                            data-effect="mom_llama.effects.consult_read.v1"
                            data-action="consult-group-insert"
                            data-handle=(group.mention_handle.clone()) {
                            span { "@" (group.mention_handle) }
                            small { (group.name) " · " (group.persona_ids.len()) " members" }
                        }
                    }
                }
                button type="button" class="text-button"
                    data-affordance=(settings_open.affordance)
                    data-command=(settings_open.command)
                    data-tauri-command=(settings_open.tauri_command)
                    data-cli=(settings_open.cli)
                    data-effect=(settings_open.effect)
                    data-action="consult-settings-open" { "Manage groups in Settings" }
            }
        }
    }
}

fn message_row(
    message: &Message,
    settings: &CommandResult<Settings>,
    attachments: &[&AttachmentRecord],
    allow_freeze: bool,
    allow_generation_actions: bool,
) -> Markup {
    let role = role_label(&message.role);
    let show_stats = upstream_settings_bool(settings, "showMessageStats");
    let enable_continue = upstream_settings_bool(settings, "enableContinueGeneration");
    let user_markdown = upstream_settings_bool(settings, "renderUserContentAsMarkdown");
    let render_thinking_markdown = upstream_settings_bool(settings, "renderThinkingAsMarkdown");
    let expand_tool_content = upstream_settings_bool(settings, "alwaysShowToolCallContent");
    let show_agentic_stats = show_stats && upstream_settings_bool(settings, "showAgenticTurnStats");
    let show_raw_output_switch = upstream_settings_bool(settings, "showRawOutputSwitch");
    let model = model_chip_label(settings);
    html! {
        article id=(format!("message-{}", message.id))
            class=(format!("message-row {role}"))
            data-message-id=(message.id.clone())
            aria-label=(format!("{role} message")) {
            div class="message-card" {
                @if let Some(attribution) = &message.attribution {
                    p class="message-attribution"
                        data-invocation=(attribution.invocation_id.clone())
                        data-source=(attribution.source_id.clone()) {
                        span class="attribution-avatar" { (icon_markup(match attribution.kind {
                            mom_llama_runtime::MessageSpeakerKind::Persona => "user-round",
                            mom_llama_runtime::MessageSpeakerKind::LiveChat => "message-circle",
                            mom_llama_runtime::MessageSpeakerKind::Synthesis => "sparkles",
                        })) }
                        strong { (attribution.label.clone()) }
                        span { "@" (attribution.handle.clone()) }
                    }
                }
                @if !attachments.is_empty() {
                    div class="message-attachments" aria-label="Message attachments" {
                        @for attachment in attachments {
                            (attachment_preview_card(attachment))
                        }
                    }
                }
                @if message.role == MessageRole::Tool {
                    (tool_message_content(message, expand_tool_content, show_agentic_stats))
                } @else if message.role == MessageRole::User && !user_markdown {
                    p class="plain-message-content" { (message.content) }
                } @else {
                    @if message.role == MessageRole::Assistant {
                        @if let Some(reasoning) = message
                            .reasoning_content
                            .as_deref()
                            .filter(|reasoning| !reasoning.trim().is_empty())
                        {
                            section class="message-reasoning" {
                                p class="message-reasoning-label" {
                                    @if message.reasoning_incomplete {
                                        "Reasoning in progress"
                                    } @else {
                                        "Reasoning"
                                    }
                                }
                                div class="reasoning-content" {
                                    @if render_thinking_markdown {
                                        (markdown_content(reasoning))
                                    } @else {
                                        p class="plain-message-content" { (reasoning) }
                                    }
                                }
                            }
                        }
                    }
                    (markdown_content(&message.content))
                    @if message.role == MessageRole::Assistant && show_raw_output_switch {
                        pre class="raw-message-content is-hidden" { (message.content.clone()) }
                    }
                }
                @if message.role == MessageRole::Assistant && show_stats {
                    p class="message-model" {
                        (model)
                        @if message.prompt_tokens.is_some() || message.completion_tokens.is_some() {
                            " · " (message.prompt_tokens.unwrap_or_default()) " prompt · "
                            (message.completion_tokens.unwrap_or_default()) " generated"
                        }
                    }
                }
            }
            (message_actions(
                message,
                allow_freeze,
                allow_generation_actions,
                enable_continue,
                show_raw_output_switch,
            ))
        }
    }
}

fn attachment_preview_card(attachment: &AttachmentRecord) -> Markup {
    let control = control("attachment.preview");
    html! {
        figure class=(format!("attachment-preview {:?}", attachment.kind).to_lowercase())
            data-attachment-preview=(attachment.id.clone())
            data-attachment-kind=(format!("{:?}", attachment.kind).to_lowercase())
            data-attachment-mime=(attachment.mime.clone())
            data-affordance=(control.affordance)
            data-command=(control.command)
            data-tauri-command=(control.tauri_command)
            data-cli=(control.cli)
            data-effect=(control.effect) {
            div class="attachment-preview-body" aria-live="polite" {
                (icon_markup(attachment_icon(&attachment.kind)))
            }
            figcaption {
                strong { (attachment.file_name.clone()) }
                small { (attachment_summary(attachment)) }
            }
        }
    }
}

fn attachment_icon(kind: &AttachmentKind) -> &'static str {
    match kind {
        AttachmentKind::Image => "image",
        AttachmentKind::Audio => "audio-lines",
        AttachmentKind::Video => "film",
        AttachmentKind::Pdf | AttachmentKind::Text => "file-text",
        AttachmentKind::Other => "file",
    }
}

fn attachment_summary(attachment: &AttachmentRecord) -> String {
    format!(
        "{} · {}",
        attachment.mime,
        human_bytes(Some(attachment.bytes))
    )
}

fn tool_message_content(message: &Message, expand: bool, show_stats: bool) -> Markup {
    let content = message.content.as_str();
    let Ok(payload) = serde_json::from_str::<Value>(content) else {
        return html! {
            article class="tool-result-card tool-result-unstructured" {
                header {
                    (icon_markup("wrench"))
                    div {
                        strong { "Local tool result" }
                        span { "Stored transcript" }
                    }
                }
                (markdown_content(content))
            }
        };
    };
    let server = payload
        .get("server")
        .and_then(Value::as_str)
        .unwrap_or("local");
    let tool = payload
        .get("tool")
        .and_then(Value::as_str)
        .unwrap_or("tool");
    let arguments = payload.get("arguments").cloned().unwrap_or(Value::Null);
    let result = payload.get("result").cloned().unwrap_or(Value::Null);
    let result_text = tool_result_text(&result)
        .unwrap_or_else(|| "The tool returned structured local data.".to_string());
    let hash = payload
        .get("result_sha256")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let short_hash = hash.chars().take(12).collect::<String>();
    let turn = payload.get("turn").and_then(Value::as_u64);
    let pretty_arguments =
        serde_json::to_string_pretty(&arguments).unwrap_or_else(|_| "{}".to_string());
    let pretty_payload =
        serde_json::to_string_pretty(&payload).unwrap_or_else(|_| content.to_string());
    let details_control = control("message.raw_toggle");
    html! {
        article class="tool-result-card" {
            header {
                (icon_markup("wrench"))
                div {
                    strong { (tool) }
                    span { (server) " · completed locally" }
                }
                span class="tool-result-state" { (icon_markup("check")) "Done" }
            }
            section class="tool-result-body" {
                p class="tool-result-label" { "Result" }
                (markdown_content(&result_text))
            }
            @if arguments != Value::Null && arguments.as_object().is_none_or(|value| !value.is_empty()) {
                section class=(format!("tool-result-details{}", if expand { "" } else { " is-hidden" })) {
                    p class="tool-result-details-label" { "Arguments" }
                    pre { code { (pretty_arguments) } }
                }
            }
            section class=(format!("tool-result-details{}", if expand { "" } else { " is-hidden" })) {
                p class="tool-result-details-label" { "Technical details" }
                pre { code { (pretty_payload) } }
            }
            button type="button" class="text-button tool-details-toggle"
                aria-expanded=(if expand { "true" } else { "false" })
                data-affordance=(details_control.affordance)
                data-command=(details_control.command)
                data-tauri-command=(details_control.tauri_command)
                data-cli=(details_control.cli)
                data-effect=(details_control.effect)
                data-action="tool-details-toggle"
                data-message=(message.id.clone()) {
                span class="button-label" { @if expand { "Hide details" } @else { "Details" } }
            }
            @if !short_hash.is_empty() {
                footer { "Result fingerprint " code { (short_hash) } }
            }
            @if show_stats {
                p class="tool-turn-stats" {
                    @if let Some(turn) = turn { "Turn " (turn) " · " }
                    (message.prompt_tokens.unwrap_or_default()) " prompt tokens · "
                    (message.completion_tokens.unwrap_or_default()) " completion tokens"
                }
            }
        }
    }
}

fn tool_result_text(result: &Value) -> Option<String> {
    let content = result.get("content").unwrap_or(result);
    match content {
        Value::String(value) if !value.trim().is_empty() => Some(value.clone()),
        Value::Array(items) => {
            let text = items
                .iter()
                .filter_map(|item| {
                    item.get("text")
                        .and_then(Value::as_str)
                        .or_else(|| item.as_str())
                })
                .collect::<Vec<_>>()
                .join("\n");
            (!text.trim().is_empty()).then_some(text)
        }
        Value::Object(object) => object
            .get("text")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string),
        _ => None,
    }
}

fn message_actions(
    message: &Message,
    allow_freeze: bool,
    allow_generation_actions: bool,
    enable_continue: bool,
    show_raw_output_switch: bool,
) -> Markup {
    html! {
        div class="message-actions" aria-label="Message actions" {
            (message_button("message.copy", "message-copy", message))
            @if message.role == MessageRole::Assistant && show_raw_output_switch {
                (message_button("message.raw_toggle", "message-raw-toggle", message))
            }
            (message_button("message.edit", "message-edit", message))
            @if allow_freeze {
                (message_button("persona.freeze", "persona-freeze", message))
            }
            @if allow_generation_actions {
                (message_button("chat.message.regenerate", "chat-regenerate", message))
                @if enable_continue {
                    (message_button("chat.message.continue", "chat-continue", message))
                }
            }
            @if message.branch_count.unwrap_or(1) > 1 {
                (message_branch_controls(message))
            }
            (message_button("message.delete", "message-delete", message))
        }
    }
}

fn message_branch_controls(message: &Message) -> Markup {
    let index = message.branch_index.unwrap_or(1);
    let count = message.branch_count.unwrap_or(1);
    let previous = control("message.branch.previous");
    let next = control("message.branch.next");
    html! {
        span class="message-branch-navigation" aria-label="Message branches" {
            button type="button"
                class="message-action branch-arrow"
                title=(previous.label)
                data-affordance=(previous.affordance)
                data-command=(previous.command)
                data-tauri-command=(previous.tauri_command)
                data-cli=(previous.cli)
                data-effect=(previous.effect)
                data-action="message-branch-step"
                data-direction="-1"
                data-message=(message.id.clone())
                disabled[index <= 1] {
                (icon_markup("chevron-left"))
                span class="sr-only" { (previous.label) }
            }
            span class="branch-position" { (index) " / " (count) }
            button type="button"
                class="message-action branch-arrow"
                title=(next.label)
                data-affordance=(next.affordance)
                data-command=(next.command)
                data-tauri-command=(next.tauri_command)
                data-cli=(next.cli)
                data-effect=(next.effect)
                data-action="message-branch-step"
                data-direction="1"
                data-message=(message.id.clone())
                disabled[index >= count] {
                (icon_markup("chevron-right"))
                span class="sr-only" { (next.label) }
            }
        }
    }
}

fn message_button(key: &str, action: &str, message: &Message) -> Markup {
    let control = control(key);
    let icon = match action {
        "message-copy" => "copy",
        "message-raw-toggle" => "code",
        "message-edit" => "pencil",
        "persona-freeze" => "snowflake",
        "chat-regenerate" => "rotate-ccw",
        "chat-continue" => "skip-forward",
        "message-delete" => "trash-2",
        _ => "circle-ellipsis",
    };
    html! {
        button type="button"
            class="message-action"
            title=(control.label)
            data-affordance=(control.affordance)
            data-command=(control.command)
            data-tauri-command=(control.tauri_command)
            data-cli=(control.cli)
            data-effect=(control.effect)
            data-action=(action)
            data-message=(message.id.clone())
            data-message-content=(message.content.clone()) {
            (icon_markup(icon))
            span class="sr-only" { (control.label) }
        }
    }
}

fn settings_modal(
    settings: &CommandResult<Settings>,
    engine: &CommandResult<impl Serialize>,
    models: &CommandResult<Vec<ModelInfo>>,
    skills: &CommandResult<Vec<Skill>>,
    kv: &CommandResult<KvCacheStatus>,
    active: Option<&Conversation>,
) -> Markup {
    let current_conversation_id = active
        .map(|conversation| conversation.id.as_str())
        .unwrap_or("default");
    html! {
        div id="settings-modal" class="modal-backdrop is-hidden" hidden[true] aria-hidden="true"
            data-current-conversation=(current_conversation_id) {
            section class="settings-dialog" aria-label="Settings" {
                div class="settings-sections" {
                    h2 { "llama.cpp" }
                    @for section in SETTINGS_SECTIONS {
                        button type="button"
                            class=(if section.slug == "general" { "section-tab active" } else { "section-tab" })
                            data-affordance="settings.section"
                            data-command="mom_llama.settings_get"
                            data-tauri-command="mom_llama_settings_get"
                            data-cli="mom-llama settings get --json"
                            data-effect="mom_llama.effects.settings_store.v1"
                            data-action="settings-section"
                            data-section=(section.slug) {
                            (icon_markup(section.icon))
                            span { (section.title) }
                        }
                    }
                }
                div class="settings-content" {
                    div class="modal-title-row" {
                        h2 id="settings-section-title" { "General" }
                        (button("settings.close", Some("settings-close"), "icon-button", false))
                    }
                    form id="settings-form" class="settings-form"
                        data-affordance="settings.update.form"
                        data-command="mom_llama.settings_update"
                        data-tauri-command="mom_llama_settings_update"
                        data-cli="mom-llama settings update"
                        data-effect="mom_llama.effects.settings_store.v1" {
                        @for section in SETTINGS_SECTIONS {
                            (settings_panel(section, settings, active))
                        }
                    }
                    div class="settings-subgrid" {
                            section class="settings-card" data-settings-card="models" {
                            h3 { "Models" }
                            (button("model.list", Some("model-list"), "small-button", false))
                            @for model in models.result.as_deref().unwrap_or(&[]) {
                                button type="button"
                                    class=(format!("model-row {}", if model.selected { "active" } else { "" }))
                                    data-affordance="readiness.model_select"
                                    data-command="mom_llama.model_select"
                                    data-tauri-command="mom_llama_model_select"
                                    data-cli="mom-llama model select --model-path <path> --json"
                                    data-effect="mom_llama.effects.model_select.v1"
                                    data-action="model-select"
                                    data-model-path=(model.path.clone())
                                    disabled[model.selected] {
                                    span { (model.id.clone()) }
                                    small { (human_bytes(model.size_bytes)) }
                                }
                            }
                        }
                            section class="settings-card" data-settings-card="skills" {
                            h3 { "Skills" }
                            (button("skills.list", Some("refresh"), "small-button", false))
                            @for skill in skills.result.as_deref().unwrap_or(&[]) {
                                div class="skill-entry" {
                                    button type="button"
                                        class="skill-row"
                                        data-affordance="skills.apply"
                                        data-command="mom_llama.skill_apply"
                                        data-tauri-command="mom_llama_skill_apply"
                                        data-cli="mom-llama skill apply --conversation <id> --skill <id-or-name> --json"
                                        data-effect="mom_llama.effects.skill_store.v1"
                                        data-action="skill-apply"
                                        data-skill=(skill.id.clone()) {
                                        span { (skill.name.clone()) }
                                        small { (skill.description.clone()) }
                                    }
                                    button type="button" class="icon-button skill-edit-button"
                                        aria-label=(format!("Edit {}", skill.name))
                                        data-affordance="skills.update"
                                        data-command="mom_llama.skill_update"
                                        data-tauri-command="mom_llama_skill_update"
                                        data-cli="mom-llama skill edit --skill <id> --name <name> --prompt-template <text> --json"
                                        data-effect="mom_llama.effects.skill_store.v1"
                                        data-action="skill-edit"
                                        data-skill=(skill.id.clone())
                                        data-skill-name=(skill.name.clone())
                                        data-skill-description=(skill.description.clone())
                                        data-skill-prompt=(skill.prompt_template.clone())
                                        data-skill-usage=(skill.usage_hint.clone())
                                        data-skill-cache=(kv_policy_value(&skill.cache_policy)) {
                                        (icon_markup("pencil"))
                                    }
                                }
                            }
                            form id="skill-form" class="skill-form"
                                data-affordance="skills.create.form"
                                data-command="mom_llama.skill_create"
                                data-tauri-command="mom_llama_skill_create"
                                data-cli="mom-llama skill create"
                                data-effect="mom_llama.effects.skill_store.v1" {
                                input type="hidden" name="skill_id"
                                    data-affordance="skills.update"
                                    data-command="mom_llama.skill_update"
                                    data-tauri-command="mom_llama_skill_update"
                                    data-cli="mom-llama skill edit --skill <id> --json"
                                    data-effect="mom_llama.effects.skill_store.v1";
                                input name="name" placeholder="Friendly explainer" aria-label="Skill name"
                                    data-affordance="skills.create.name"
                                    data-command="mom_llama.skill_create"
                                    data-tauri-command="mom_llama_skill_create"
                                    data-cli="mom-llama skill create"
                                    data-effect="mom_llama.effects.skill_store.v1";
                                input name="description" placeholder="Explain gently" aria-label="Skill description"
                                    data-affordance="skills.create.description"
                                    data-command="mom_llama.skill_create"
                                    data-tauri-command="mom_llama_skill_create"
                                    data-cli="mom-llama skill create"
                                    data-effect="mom_llama.effects.skill_store.v1";
                                textarea name="prompt_template" rows="3" placeholder="Explain this in simple, friendly language:" aria-label="Skill prompt template"
                                    data-affordance="skills.create.prompt_template"
                                    data-command="mom_llama.skill_create"
                                    data-tauri-command="mom_llama_skill_create"
                                    data-cli="mom-llama skill create"
                                    data-effect="mom_llama.effects.skill_store.v1" {}
                                select name="cache_policy" aria-label="Cache policy"
                                    data-affordance="skills.create.cache_policy"
                                    data-command="mom_llama.skill_create"
                                    data-tauri-command="mom_llama_skill_create"
                                    data-cli="mom-llama skill create"
                                    data-effect="mom_llama.effects.skill_store.v1" {
                                    option value="none" { "Do not reuse" }
                                    option value="prompt_prefix" { "Reuse this prompt" }
                                }
                                (button("skills.create", Some("skill-create"), "small-button primary", false))
                                button type="button" class="small-button is-hidden" data-action="skill-edit-cancel"
                                    data-affordance="skills.list"
                                    data-command="mom_llama.skill_list"
                                    data-tauri-command="mom_llama_skill_list"
                                    data-cli="mom-llama skill list --json"
                                    data-effect="mom_llama.effects.skill_store.v1" { "Cancel edit" }
                            }
                        }
                            section class="settings-card" data-settings-card="cache" {
                            h3 { "Prompt cache" }
                            p { (kv_label(kv)) }
                            div class="button-strip" {
                                (button("kv.status", Some("kv-status"), "small-button", false))
                                (button("kv.save", Some("kv-save"), "small-button", false))
                                (button("kv.restore", Some("kv-restore"), "small-button", false))
                                (button("kv.clear", Some("kv-clear"), "small-button danger", false))
                            }
                        }
                            section class="settings-card" data-settings-card="engine" {
                            h3 { "Engine" }
                            p { (readiness_short_label(engine)) }
                            div class="button-strip" {
                                (button("readiness.engine_check", Some("engine-check"), "small-button", false))
                                (button("mcp.status", Some("mcp-status"), "small-button", false))
                            }
                        }
                    }
                        p class="receipt-panel" id="settings-receipt" { (readiness_short_label(engine)) }
                }
                footer class="settings-footer" {
                    (button("settings.reset", Some("settings-reset"), "small-button", false))
                    div class="settings-autosave" data-state="idle" {
                        span class="settings-save-glyph settings-save-glyph-saving" aria-hidden="true" {
                            (icon_markup("loader-circle"))
                        }
                        span class="settings-save-glyph settings-save-glyph-saved" aria-hidden="true" {
                            (icon_markup("check"))
                        }
                        span class="settings-save-error" aria-hidden="true" {
                            (icon_markup("alert-triangle"))
                            "Couldn’t save"
                        }
                        span id="settings-save-status" class="sr-only" role="status" aria-live="polite" {
                            "Changes save automatically"
                        }
                        (button("settings.update", Some("settings-retry"), "small-button settings-retry is-hidden", false))
                    }
                }
            }
        }
    }
}

fn settings_panel(
    section: &SettingsSectionSpec,
    settings: &CommandResult<Settings>,
    active: Option<&Conversation>,
) -> Markup {
    html! {
        section class=(if section.slug == "general" { "settings-panel active" } else { "settings-panel" })
            data-section-panel=(section.slug)
            aria-label=(section.title) {
            div class="settings-panel-heading" {
                (icon_markup(section.icon))
                h3 { (section.title) }
            }
            @if let Some(blocker) = section.blocker {
                p class="settings-blocker"
                    data-blocker-code=(format!("{}_blocked_native_profile", section.slug.replace('-', "_"))) {
                    (blocker)
                }
            }
            @if section.slug == "general" {
                (current_chat_instructions(active))
                section class="settings-card native-runtime" {
                    h3 { "Native runtime" }
                    (settings_path_input("Model path", "model_path", settings_value(settings, "model_path"), "model-browse"))
                    @if let Some(cache) = mom_llama_runtime::hugging_face_hub_cache_dir() {
                        p class="field-help model-cache-hint" {
                            "Model discovery and the file picker use the shared Hugging Face cache at "
                            code { (cache.display()) }
                            "."
                        }
                    }
                    (settings_path_input("Multimodal projector", "mmproj_path", settings_value(settings, "mmproj_path"), "mmproj-browse"))
                    label class="field" { span { "Device" }
                        select name="native_device" data-setting-core="native_device"
                            data-affordance="settings.update" data-command="mom_llama.settings_update"
                            data-tauri-command="mom_llama_settings_update"
                            data-cli="mom-llama settings update --device <auto|cpu|metal> --json"
                            data-effect="mom_llama.effects.settings_store.v1" {
                            option value="auto" selected[settings_value(settings, "native_device") == "auto"] { "Automatic" }
                            option value="metal" selected[settings_value(settings, "native_device") == "metal"] { "Metal" }
                            option value="cpu" selected[settings_value(settings, "native_device") == "cpu"] { "CPU" }
                        }
                    }
                    div class="native-number-grid" {
                        (settings_input("Context tokens", "context_tokens", settings_value(settings, "context_tokens")))
                        (settings_input("Batch tokens", "batch_tokens", settings_value(settings, "batch_tokens")))
                        (settings_input("Parallel sequences", "max_parallel_sequences", settings_value(settings, "max_parallel_sequences")))
                        (settings_input("Memory budget (MiB)", "memory_budget_mib", settings_value(settings, "memory_budget_mib")))
                    }
                    p class="field-help" { "The GGUF model is loaded directly inside this app. No executable or local server is used." }
                }
            }
            @if section.slug == "consult" {
                (consult_settings())
            }
            @if section.slug == "personas" {
                (persona_settings())
            }
            @if section.slug == "import-export" {
                section class="settings-card" {
                    h3 { "Conversations" }
                    p class="field-help" { "Import and export use the Rust-owned conversation store, not IndexedDB." }
                    div class="button-strip" {
                        (button("conversation.export", Some("conversation-export"), "small-button", false))
                        (button("conversation.import", Some("conversation-import"), "small-button", false))
                    }
                }
            }
            @if section.slug == "tools" {
                section class="settings-card" {
                    h3 { "Tool permissions" }
                    p class="field-help" {
                        "Choose whether each configured local tool asks every time, runs automatically, or is denied. Revoking returns it to Ask."
                    }
                    div class="native-number-grid" {
                        (command_input("Server name", "permission_server", "", "tool_permission.set"))
                        (command_input("Tool name", "permission_tool", "", "tool_permission.set"))
                        label class="field" {
                            span { "Policy" }
                            select name="permission_policy"
                                data-affordance="tool_permission.set"
                                data-command="mom_llama.tool_permission_set"
                                data-tauri-command="mom_llama_tool_permission_set"
                                data-cli="mom-llama tool-loop permission-set --server <name> --tool <name> --policy <ask|always-allow|deny> --json"
                                data-effect="mom_llama.effects.tool_permission_store.v1" {
                                option value="ask" { "Ask every time" }
                                option value="always_allow" { "Always allow" }
                                option value="deny" { "Deny" }
                            }
                        }
                    }
                    div class="button-strip" {
                        (button("tool_permission.list", Some("tool-permission-list"), "small-button", false))
                        (button("tool_permission.revoke", Some("tool-permission-revoke"), "small-button", false))
                        (button("tool_permission.set", Some("tool-permission-set"), "primary-button", false))
                    }
                }
            }
            div class="settings-field-list" {
                @for field in SETTINGS_FIELDS.iter().filter(|field| field.section == section.slug) {
                    (settings_field(field, settings))
                }
                @for field in NATIVE_SETTINGS_FIELDS.iter().filter(|field| field.section == section.slug) {
                    (settings_field(field, settings))
                }
            }
            @if section.slug == "mcp" {
                    section class="settings-card adapter-form" {
                        h3 { "Native tool adapter" }
                        div class="native-number-grid" {
                            (command_input("Server name", "mcp_server", "", "mcp.configure"))
                            (command_input("Tool name", "mcp_tool", "", "mcp.call_tool"))
                            (command_input("Resource URI", "mcp_uri", "", "mcp.read_resource"))
                            (command_input("Prompt name", "mcp_prompt", "", "mcp.get_prompt"))
                        }
                        (command_path_input("Executable", "mcp_command", "", "mcp-command-browse", "mcp.configure"))
                        label class="field" { span { "Arguments (JSON)" }
                            textarea name="mcp_arguments" rows="3"
                                data-affordance="mcp.call_tool" data-command="mom_llama.mcp_call_tool"
                                data-tauri-command="mom_llama_mcp_call_tool"
                                data-cli="mom-llama mcp call-tool --arguments <json> --json"
                                data-effect="mom_llama.effects.mcp_stdio.v1" { "{}" }
                        }
                        label class="field" { span { "Consult prompt" }
                            textarea name="tool_loop_prompt" rows="3"
                                data-affordance="tool_loop.prepare" data-command="mom_llama.tool_loop_prepare"
                                data-tauri-command="mom_llama_tool_loop_prepare"
                                data-cli="mom-llama tool-loop prepare --prompt <text> --json"
                                data-effect="mom_llama.effects.tool_loop.v1" {}
                        }
                    }
                    div class="button-strip" {
                        (button("mcp.status", Some("mcp-status"), "small-button", false))
                        (button("mcp.configure", Some("mcp-configure"), "small-button", false))
                        (button("mcp.list_servers", Some("mcp-list-servers"), "small-button", false))
                        (button("mcp.list_tools", Some("mcp-list-tools"), "small-button", false))
                        (button("mcp.list_resources", Some("mcp-list-resources"), "small-button", false))
                        (button("mcp.read_resource", Some("mcp-read-resource"), "small-button", false))
                        (button("mcp.list_prompts", Some("mcp-list-prompts"), "small-button", false))
                        (button("mcp.get_prompt", Some("mcp-get-prompt"), "small-button", false))
                        (button("mcp.call_tool", Some("mcp-call-tool"), "small-button", false))
                        (button("tool_loop.prepare", Some("tool-loop-prepare"), "primary-button", false))
                    }
            }
            @if section.slug == "developer" {
                section class="settings-card cache-preferences" {
                    h3 { "Prompt caching" }
                    label class="field" {
                        span { "Mode" }
                        select name="kv_cache_policy" data-setting-core="kv_cache_policy"
                            data-affordance="settings.update"
                            data-command="mom_llama.settings_update"
                            data-tauri-command="mom_llama_settings_update"
                            data-cli="mom-llama settings update --kv-cache-policy <automatic|prefixes-only|off> --json"
                            data-effect="mom_llama.effects.settings_store.v1" {
                            option value="kv_cache_candidate"
                                selected[settings_value(settings, "kv_cache_policy") == "kv_cache_candidate"] {
                                "Automatic (recommended)"
                            }
                            option value="prompt_prefix"
                                selected[settings_value(settings, "kv_cache_policy") == "prompt_prefix"] {
                                "Prefixes only"
                            }
                            option value="none"
                                selected[settings_value(settings, "kv_cache_policy") == "none"] {
                                "Off"
                            }
                        }
                    }
                    p class="field-help" {
                        "Automatic keeps reusable Persona and Skill prefixes and an encrypted checkpoint for each active conversation. Prefixes only skips conversation checkpoints. Off neither creates nor reads cached prompt state."
                    }
                    p class="field-help" {
                        "Safety limits remain fixed at 256 MiB in memory, 64 persistent entries, and 2 GiB on disk. Incompatible state is discarded and generation continues normally."
                    }
                }
                section class="settings-card" {
                    h3 { "Resident models" }
                    p class="field-help" { "Model slots are native owner-thread workers governed by the app memory budget." }
                    div class="native-number-grid resident-fields" {
                        (command_input("Slot", "resident_slot", "0", "resident.slot_load"))
                        (command_path_input("Model path", "resident_model_path", &settings_value(settings, "model_path"), "resident-model-browse", "resident.slot_load"))
                    }
                    div class="button-strip" {
                        (button("resident.slots", Some("resident-slots"), "small-button", false))
                        (button("resident.slot_load", Some("resident-slot-load"), "small-button", false))
                        (button("resident.slot_unload", Some("resident-slot-unload"), "small-button", false))
                    }
                }
            }
        }
    }
}

fn current_chat_instructions(active: Option<&Conversation>) -> Markup {
    let conversation_id = active
        .map(|conversation| conversation.id.as_str())
        .unwrap_or("default");
    let title = active
        .map(|conversation| conversation.title.as_str())
        .unwrap_or("New chat");
    let system_message = active
        .and_then(|conversation| conversation.execution_profile.system_message.as_deref())
        .unwrap_or_default();
    html! {
        section class="settings-card current-chat-settings" {
            div class="current-chat-heading" {
                div {
                    h3 { "Current chat instructions" }
                    p { (title) }
                }
                span class="scope-chip" { "This chat only" }
            }
            label class="field" {
                span class="sr-only" { "Current chat system message" }
                textarea name="conversation_system_message" rows="4"
                    placeholder="Use the default system message"
                    data-chat-setting="system_message"
                    data-conversation=(conversation_id)
                    data-affordance="conversation.system_message"
                    data-command="mom_llama.conversation_system_message_update"
                    data-tauri-command="mom_llama_conversation_system_message_update"
                    data-cli="mom-llama conversation system-message --conversation <id> --message <text> --json"
                    data-effect="mom_llama.effects.conversation_store.v1" {
                    (system_message)
                }
                small class="field-help" {
                    "Leave blank to inherit the default below. Changes apply to future replies in this conversation."
                }
            }
        }
    }
}

fn persona_settings() -> Markup {
    let StoreProjection {
        value: personas,
        blocker,
    } = store_projection(
        mom_llama_runtime::persona_list(),
        "persona_store_unavailable",
        "Saved Personas could not be loaded from local storage.",
    );
    let select = control("conversation.select");
    html! {
        section class="settings-card persona-library" {
            h3 { "Personas" }
            p class="field-help" {
                "Open a Persona to edit its transcript, or use the pencil for its model and context profile."
            }
            div id="persona-list" class="persona-list" {
                @if let Some(blocker) = &blocker {
                    (store_blocker(blocker))
                } @else if personas.is_empty() {
                    p class="empty-line" { "Freeze any message from its context menu to create a persona." }
                }
                @for persona in &personas {
                    div class="persona-row" {
                        button type="button" class="persona-select"
                            data-affordance=(select.affordance)
                            data-command=(select.command)
                            data-tauri-command=(select.tauri_command)
                            data-cli=(select.cli)
                            data-effect=(select.effect)
                            data-action="persona-open" data-conversation=(persona.id.clone()) {
                            span { (persona.title.clone()) }
                            small { "@" (persona.execution_profile.mention_handle.clone()) }
                        }
                        button type="button" class="icon-button"
                            aria-label=(format!("Edit {} profile", persona.title))
                            data-affordance="persona.get"
                            data-command="mom_llama.persona_get"
                            data-tauri-command="mom_llama_persona_get"
                            data-cli="mom-llama persona get --persona <id> --json"
                            data-effect="mom_llama.effects.conversation_store.v1"
                            data-action="persona-edit"
                            data-persona=(persona.id.clone())
                            data-persona-json=(serde_json::to_string(persona).unwrap_or_else(|_| "{}".to_string())) {
                            (icon_markup("pencil"))
                        }
                    }
                }
            }
        }
        section id="persona-editor" class="settings-card persona-editor is-hidden" {
            h3 { "Persona profile" }
            input type="hidden" name="persona_id"
                data-affordance="persona.update" data-command="mom_llama.persona_update"
                data-tauri-command="mom_llama_persona_update"
                data-cli="mom-llama persona update --profile <json> --json"
                data-effect="mom_llama.effects.conversation_store.v1";
            div class="native-number-grid" {
                (command_input("Name", "persona_name", "", "persona.update"))
                (command_input("@handle", "persona_handle", "", "persona.update"))
                (command_path_input("Model", "persona_model_path", "", "persona-model-browse", "persona.update"))
                (command_path_input("Projector", "persona_mmproj_path", "", "persona-mmproj-browse", "persona.update"))
            }
            label class="field" { span { "System message (optional)" }
                textarea name="persona_system_message" rows="4"
                    data-affordance="persona.update" data-command="mom_llama.persona_update"
                    data-tauri-command="mom_llama_persona_update"
                    data-cli="mom-llama persona update --profile <json> --json"
                    data-effect="mom_llama.effects.conversation_store.v1" {}
            }
            label class="field" { span { "Chat template policy" }
                select name="persona_chat_template_policy"
                    data-affordance="persona.update" data-command="mom_llama.persona_update"
                    data-tauri-command="mom_llama_persona_update"
                    data-cli="mom-llama persona update --profile <json> --json"
                    data-effect="mom_llama.effects.conversation_store.v1" {
                    option value="model_default" { "Selected model default" }
                    option value="frozen_source" { "Frozen template override" }
                }
            }
            label class="field persona-template-source is-hidden" { span { "Frozen chat template" }
                textarea name="persona_chat_template" rows="4"
                    data-affordance="persona.update" data-command="mom_llama.persona_update"
                    data-tauri-command="mom_llama_persona_update"
                    data-cli="mom-llama persona update --profile <json> --json"
                    data-effect="mom_llama.effects.conversation_store.v1" {}
            }
            div class="native-number-grid" {
                (command_input("Persona history tokens", "persona_source_tokens", "4096", "persona.update"))
                (command_input("Host context tokens", "persona_host_tokens", "2048", "persona.update"))
            }
            label class="field" { span { "Attached tools" }
                textarea name="persona_tools" rows="3" placeholder="server/tool, one per line"
                    data-affordance="persona.update" data-command="mom_llama.persona_update"
                    data-tauri-command="mom_llama_persona_update"
                    data-cli="mom-llama persona update --profile <json> --json"
                    data-effect="mom_llama.effects.conversation_store.v1" {}
                small { "Only these stable tool bindings may be offered during an invited response." }
            }
            div class="button-strip" {
                (button("persona.update", Some("persona-update"), "primary-button", false))
                (button("persona.delete", Some("persona-delete"), "small-button danger", false))
            }
        }
    }
}

fn consult_settings() -> Markup {
    let StoreProjection {
        value: personas,
        blocker: persona_blocker,
    } = store_projection(
        mom_llama_runtime::persona_list(),
        "persona_store_unavailable",
        "Saved Personas could not be loaded from local storage.",
    );
    let StoreProjection {
        value: groups,
        blocker: group_blocker,
    } = store_projection(
        mom_llama_runtime::persona_group_list(),
        "persona_group_store_unavailable",
        "Consult groups could not be loaded from local storage.",
    );
    html! {
        section class="settings-card persona-groups" {
            h3 { "Consult groups" }
            p class="field-help" { "Groups are ordered references to one to four Personas. They never duplicate persona definitions." }
            div id="persona-group-list" class="persona-group-list" {
                @if let Some(blocker) = &group_blocker {
                    (store_blocker(blocker))
                } @else if groups.is_empty() {
                    p class="empty-line" { "No consult groups yet." }
                }
                @for group in &groups {
                    div class="persona-group-row" {
                        button type="button" class="persona-select"
                            data-affordance="persona_group.list"
                            data-command="mom_llama.persona_group_list"
                            data-tauri-command="mom_llama_persona_group_list"
                            data-cli="mom-llama persona-group list --json"
                            data-effect="mom_llama.effects.consult_read.v1"
                            data-action="persona-group-edit"
                            data-group-json=(serde_json::to_string(group).unwrap_or_else(|_| "{}".to_string())) {
                            span { (group.name.clone()) }
                            small { "@" (group.mention_handle.clone()) " · " (group.persona_ids.len()) " members" }
                        }
                        button type="button" class="icon-button danger"
                            aria-label=(format!("Delete {}", group.name))
                            data-affordance="persona_group.delete"
                            data-command="mom_llama.persona_group_delete"
                            data-tauri-command="mom_llama_persona_group_delete"
                            data-cli="mom-llama persona-group delete --group <id> --json"
                            data-effect="mom_llama.effects.consult_store.v1"
                            data-action="persona-group-delete" data-group=(group.id.clone()) {
                            (icon_markup("trash-2"))
                        }
                    }
                }
            }
            button type="button" class="secondary-button"
                data-affordance="persona_group.create"
                data-command="mom_llama.persona_group_create"
                data-tauri-command="mom_llama_persona_group_create"
                data-cli="mom-llama persona-group create --name <name> --handle <handle> --persona <id> --json"
                data-effect="mom_llama.effects.consult_store.v1"
                data-action="persona-group-new" { (icon_markup("plus")) "New group" }
        }
        section id="persona-group-editor" class="settings-card persona-group-editor is-hidden" {
            h3 { "Group pattern" }
            @if let Some(blocker) = &persona_blocker {
                (store_blocker(blocker))
            }
            input type="hidden" name="persona_group_id"
                data-affordance="persona_group.create" data-command="mom_llama.persona_group_create"
                data-tauri-command="mom_llama_persona_group_create"
                data-cli="mom-llama persona-group create --name <name> --handle <handle> --persona <id> --json"
                data-effect="mom_llama.effects.consult_store.v1";
            div class="native-number-grid" {
                (command_input("Name", "persona_group_name", "", "persona_group.create"))
                (command_input("@handle", "persona_group_handle", "", "persona_group.create"))
            }
            @for index in 0..4 {
                label class="field" { span { "Member " (index + 1) }
                    select name=(format!("persona_group_member_{index}"))
                        data-affordance="persona_group.create"
                        data-command="mom_llama.persona_group_create"
                        data-tauri-command="mom_llama_persona_group_create"
                        data-cli="mom-llama persona-group create --persona <id> --json"
                        data-effect="mom_llama.effects.consult_store.v1" {
                        option value="" { @if index == 0 { "Choose a persona" } @else { "None" } }
                        @for persona in &personas {
                            option value=(persona.id.clone()) { (persona.title.clone()) " (@" (persona.execution_profile.mention_handle.clone()) ")" }
                        }
                    }
                }
            }
            div class="button-strip" {
                (button("persona_group.create", Some("persona-group-save"), "primary-button persona-group-create", false))
                (button("persona_group.update", Some("persona-group-save"), "primary-button persona-group-update is-hidden", false))
            }
        }
    }
}

fn persona_freeze_modal() -> Markup {
    let freeze = control("persona.freeze");
    html! {
        div id="persona-freeze-modal" class="modal-backdrop is-hidden" hidden[true] aria-hidden="true" {
            section class="compact-dialog" role="dialog" aria-modal="true" aria-labelledby="persona-freeze-title" {
                header class="modal-title-row" {
                    div { p class="eyebrow" { "PERSONA" } h2 id="persona-freeze-title" { "Freeze this branch" } }
                    button type="button" class="icon-button" aria-label="Close"
                        data-affordance="persona.list" data-command="mom_llama.persona_list"
                        data-tauri-command="mom_llama_persona_list" data-cli="mom-llama persona list --json"
                        data-effect="mom_llama.effects.conversation_store.v1" data-action="persona-freeze-close" {
                        (icon_markup("x"))
                    }
                }
                input type="hidden" name="freeze_message"
                    data-affordance=(freeze.affordance) data-command=(freeze.command)
                    data-tauri-command=(freeze.tauri_command) data-cli=(freeze.cli)
                    data-effect=(freeze.effect);
                label class="field" { span { "Name" }
                    input name="freeze_name" placeholder="Careful reviewer"
                        data-affordance=(freeze.affordance) data-command=(freeze.command)
                        data-tauri-command=(freeze.tauri_command) data-cli=(freeze.cli)
                        data-effect=(freeze.effect);
                }
                label class="field" { span { "@handle" }
                    input name="freeze_handle" placeholder="careful-reviewer"
                        data-affordance=(freeze.affordance) data-command=(freeze.command)
                        data-tauri-command=(freeze.tauri_command) data-cli=(freeze.cli)
                        data-effect=(freeze.effect);
                }
                fieldset class="history-mode" {
                    legend { "History to keep" }
                    @for (value, label, checked) in [
                        ("full", "Full branch through this message", true),
                        ("system_only", "System messages only", false),
                        ("empty", "Empty history", false),
                    ] {
                        label {
                            input type="radio" name="freeze_history" value=(value) checked[checked]
                                data-affordance=(freeze.affordance) data-command=(freeze.command)
                                data-tauri-command=(freeze.tauri_command) data-cli=(freeze.cli)
                                data-effect=(freeze.effect);
                            (label)
                        }
                    }
                }
                button type="button" class="primary-button"
                    data-affordance=(freeze.affordance) data-command=(freeze.command)
                    data-tauri-command=(freeze.tauri_command) data-cli=(freeze.cli)
                    data-effect=(freeze.effect) data-action="persona-freeze-save" {
                    (icon_markup("snowflake")) "Freeze as persona"
                }
            }
        }
    }
}

fn tool_approval_modal() -> Markup {
    html! {
        div id="tool-approval-modal" class="modal-backdrop is-hidden" hidden[true] aria-hidden="true" {
            section class="tool-approval-dialog" role="dialog" aria-modal="true"
                aria-labelledby="tool-approval-title" {
                p class="eyebrow" { "LOCAL TOOL AUTHORITY" }
                h2 id="tool-approval-title" { "Approve this tool call?" }
                p class="field-help" {
                    "Approval is single-use, expires after five minutes, and is bound to the exact call below."
                }
                dl class="tool-approval-summary" {
                    div { dt { "Server" } dd id="tool-approval-server" {} }
                    div { dt { "Tool" } dd id="tool-approval-tool" {} }
                    div { dt { "Prompt" } dd id="tool-approval-prompt" {} }
                    div { dt { "Maximum turns" } dd id="tool-approval-turns" {} }
                }
                h3 { "Arguments" }
                pre id="tool-approval-arguments" class="approval-arguments" { "{}" }
                section id="tool-loop-live" class="tool-loop-live is-hidden"
                    aria-live="polite" aria-label="Live tool activity" {
                    header {
                        (icon_markup("wrench"))
                        strong { "Local tool activity" }
                        span id="tool-loop-live-state" { "Waiting for approval" }
                    }
                    div id="tool-loop-live-events" class="tool-loop-live-events" {}
                }
                div class="button-strip approval-actions" {
                    (button("settings.close", Some("tool-approval-close"), "small-button", false))
                    (button("tool_loop.cancel", Some("tool-loop-cancel"), "small-button danger", true))
                    (button("tool_loop.run", Some("tool-loop-run"), "primary-button", true))
                }
            }
        }
    }
}

fn settings_field(field: &SettingsFieldSpec, settings: &CommandResult<Settings>) -> Markup {
    let value = upstream_settings_value(settings, field.key);
    let checked = upstream_settings_bool(settings, field.key);
    html! {
        label class="field upstream-field" data-setting-section=(field.section) {
            span { (field.label) }
            @match field.kind {
                "checkbox" => {
                    input type="checkbox"
                        name=(field.key)
                        checked[checked]
                        disabled[field.blocker.is_some()]
                        data-setting-key=(field.key)
                        data-setting-type="boolean"
                        data-affordance=(format!("settings.{}", field.key))
                        data-command="mom_llama.settings_update"
                        data-tauri-command="mom_llama_settings_update"
                        data-cli="mom-llama settings update --set key=value --json"
                        data-effect="mom_llama.effects.settings_store.v1";
                }
                "textarea" => {
                    textarea name=(field.key) rows="4"
                        disabled[field.blocker.is_some()]
                        data-setting-key=(field.key)
                        data-setting-type="string"
                        data-affordance=(format!("settings.{}", field.key))
                        data-command="mom_llama.settings_update"
                        data-tauri-command="mom_llama_settings_update"
                        data-cli="mom-llama settings update --set key=value --json"
                        data-effect="mom_llama.effects.settings_store.v1" {
                        (value)
                    }
                }
                "select" => {
                    select name=(field.key)
                        disabled[field.blocker.is_some()]
                        data-setting-key=(field.key)
                        data-setting-type="string"
                        data-affordance=(format!("settings.{}", field.key))
                        data-command="mom_llama.settings_update"
                        data-tauri-command="mom_llama_settings_update"
                        data-cli="mom-llama settings update --set key=value --json"
                        data-effect="mom_llama.effects.settings_store.v1" {
                        @for (option, label) in field.options {
                            option value=(option) selected[*option == value.as_str()] { (label) }
                        }
                    }
                }
                "password" => {
                    input type="password"
                        name=(field.key)
                        value=(value)
                        disabled[field.blocker.is_some()]
                        data-setting-key=(field.key)
                        data-setting-type="string"
                        data-affordance=(format!("settings.{}", field.key))
                        data-command="mom_llama.settings_update"
                        data-tauri-command="mom_llama_settings_update"
                        data-cli="mom-llama settings update --set key=value --json"
                        data-effect="mom_llama.effects.settings_store.v1";
                }
                "number" => {
                    input type="number" step="any"
                        name=(field.key)
                        value=(value)
                        disabled[field.blocker.is_some()]
                        data-setting-key=(field.key)
                        data-setting-type="number"
                        data-affordance=(format!("settings.{}", field.key))
                        data-command="mom_llama.settings_update"
                        data-tauri-command="mom_llama_settings_update"
                        data-cli="mom-llama settings update --set key=value --json"
                        data-effect="mom_llama.effects.settings_store.v1";
                }
                _ => {
                    input type="text"
                        name=(field.key)
                        value=(value)
                        disabled[field.blocker.is_some()]
                        data-setting-key=(field.key)
                        data-setting-type="string"
                        data-affordance=(format!("settings.{}", field.key))
                        data-command="mom_llama.settings_update"
                        data-tauri-command="mom_llama_settings_update"
                        data-cli="mom-llama settings update --set key=value --json"
                        data-effect="mom_llama.effects.settings_store.v1";
                }
            }
            small class="field-help" { (field.help) }
            @if let Some(blocker) = field.blocker {
                small class="settings-blocker" data-blocker-code=(format!("{}_blocked_native_profile", field.key)) {
                    (blocker)
                }
            }
        }
    }
}

fn settings_input(label: &str, name: &str, value: String) -> Markup {
    html! {
        label class="field" { span { (label) }
            input type="number" name=(name) value=(value)
                data-setting-core=(name)
                data-affordance=(format!("settings.{name}"))
                data-command="mom_llama.settings_update"
                data-tauri-command="mom_llama_settings_update"
                data-cli="mom-llama settings update"
                data-effect="mom_llama.effects.settings_store.v1";
        }
    }
}

fn settings_path_input(label: &str, name: &str, value: String, action: &str) -> Markup {
    let picker = control("path.select");
    html! {
        label class="field" { span { (label) }
            div class="path-field" {
                input name=(name) value=(value)
                    data-setting-core=(name)
                    data-affordance="settings.update"
                    data-command="mom_llama.settings_update"
                    data-tauri-command="mom_llama_settings_update"
                    data-cli="mom-llama settings update --json"
                    data-effect="mom_llama.effects.settings_store.v1";
                button type="button" class="icon-button" aria-label=(format!("Choose {label}"))
                    data-action=(action)
                    data-affordance=(picker.affordance)
                    data-command=(picker.command)
                    data-tauri-command=(picker.tauri_command)
                    data-cli=(picker.cli)
                    data-effect=(picker.effect) {
                    (icon_markup("folder-open"))
                }
            }
        }
    }
}

fn command_input(label: &str, name: &str, value: &str, control_key: &str) -> Markup {
    let command = control(control_key);
    html! {
        label class="field" { span { (label) }
            input name=(name) value=(value)
                data-affordance=(command.affordance)
                data-command=(command.command)
                data-tauri-command=(command.tauri_command)
                data-cli=(command.cli)
                data-effect=(command.effect);
        }
    }
}

fn command_path_input(
    label: &str,
    name: &str,
    value: &str,
    action: &str,
    control_key: &str,
) -> Markup {
    let command = control(control_key);
    let picker = control("path.select");
    html! {
        label class="field" { span { (label) }
            div class="path-field" {
                input name=(name) value=(value)
                    data-affordance=(command.affordance)
                    data-command=(command.command)
                    data-tauri-command=(command.tauri_command)
                    data-cli=(command.cli)
                    data-effect=(command.effect);
                button type="button" class="icon-button" aria-label=(format!("Choose {label}"))
                    data-action=(action)
                    data-affordance=(picker.affordance)
                    data-command=(picker.command)
                    data-tauri-command=(picker.tauri_command)
                    data-cli=(picker.cli)
                    data-effect=(picker.effect) { (icon_markup("folder-open")) }
            }
        }
    }
}

fn button(key: &str, action: Option<&str>, class_name: &str, disabled: bool) -> Markup {
    let control = control(key);
    html! {
        button type="button"
            class=(class_name)
            aria-label=(control.label)
            data-affordance=(control.affordance)
            data-command=(control.command)
            data-tauri-command=(control.tauri_command)
            data-cli=(control.cli)
            data-effect=(control.effect)
            data-action=[action]
            disabled[disabled] {
            (button_inner(control, class_name))
        }
    }
}

fn button_inner(control: &ControlSpec, class_name: &str) -> Markup {
    let icon_name = icon_for_affordance(control.affordance);
    let icon_only = class_name.contains("icon-button")
        || class_name.contains("round-button")
        || class_name.contains("send-button");
    html! {
        @if let Some(icon_name) = icon_name {
            (icon_markup(icon_name))
        }
        @if icon_only {
            span class="sr-only" { (control.label) }
        } @else {
            span { (control.label) }
        }
    }
}

fn icon_for_affordance(affordance: &str) -> Option<&'static str> {
    match affordance {
        "layout.sidebar_toggle" => Some("panel-left"),
        "settings.open" | "settings.close" | "settings.get" | "settings.section" => {
            Some("settings")
        }
        "conversation.new" => Some("square-pen"),
        "conversation.search" => Some("search"),
        "conversation.search.close" => Some("x"),
        "conversation.import" => Some("database"),
        "conversation.export" => Some("download"),
        "mcp.status" | "mcp.configure" | "mcp.list_servers" | "mcp.list_tools"
        | "mcp.call_tool" => Some("mcp"),
        "tool_loop.run" => Some("list-restart"),
        "tool_loop.cancel" => Some("square"),
        "attachment.import_text" | "attachment.import" => Some("plus"),
        "attachment.list" => Some("database"),
        "chat.composer.send" => Some("arrow-up"),
        "chat.composer.cancel" => Some("square"),
        "chat.composer.skip_reasoning" => Some("skip-forward"),
        "persona.list" => Some("user-round"),
        "consult.open" => Some("users"),
        "consult.close" => Some("x"),
        "mention.synthesize" => Some("sparkles"),
        "settings.reset" => Some("rotate-ccw"),
        "settings.update" => Some("rotate-ccw"),
        "model.list" => Some("refresh-cw"),
        "skills.list" => Some("refresh-cw"),
        "skills.create" => Some("plus"),
        "skills.update" => Some("pencil"),
        "skills.apply" => Some("pencil-ruler"),
        "kv.status" => Some("database"),
        "kv.save" => Some("save"),
        "kv.restore" => Some("rotate-ccw"),
        "kv.clear" => Some("trash-2"),
        "resident.slots" => Some("box"),
        "resident.slot_load" => Some("download"),
        "resident.slot_unload" => Some("trash-2"),
        "readiness.engine_check" => Some("check"),
        "readiness.engine_configure" => Some("settings"),
        "readiness.model_select" => Some("box"),
        _ => None,
    }
}

fn icon_markup(name: &str) -> Markup {
    if name == "mcp" {
        return html! {
            svg class="icon mcp-logo" viewBox="0 0 174 174" fill="none" aria-hidden="true" {
                (PreEscaped(r#"<path d="M15.5587158203125,81.5927734375L83.44091796875,13.7105712890625C92.813720703125,4.3380126953125,108.0096435546875,4.3380126953125,117.3817138671875,13.7105712890625C126.7547607421875,23.08306884765625,126.7547607421875,38.27911376953125,117.3817138671875,47.65167236328125L66.1168212890625,98.9169921875" stroke="currentColor" stroke-width="12" stroke-linecap="round"/><path d="M66.5587158203125,98.26885986328125L117.1165771484375,47.7105712890625C126.489501953125,38.3380126953125,141.6854248046875,38.3380126953125,151.0584716796875,47.7105712890625L151.4114990234375,48.0640869140625C160.7845458984375,57.43670654296875,160.7845458984375,72.6326904296875,151.4114990234375,82.00518798828125L90.018310546875,143.39886474609375C86.8941650390625,146.52288818359375,86.8941650390625,151.587890625,90.018310546875,154.71185302734375L102.62451171875,167.31890869140625" stroke="currentColor" stroke-width="12" stroke-linecap="round"/><path d="M99.79296875,30.68115234375L49.588134765625,80.8857421875C40.215576171875,90.258056640625,40.215576171875,105.45404052734375,49.588134765625,114.82708740234375C58.9608154296875,124.19903564453125,74.1566162109375,124.19903564453125,83.529296875,114.82708740234375L133.7340087890625,64.62225341796875" stroke="currentColor" stroke-width="12" stroke-linecap="round"/>"#))
            }
        };
    }
    html! {
        svg class=(format!("icon lucide lucide-{name}")) viewBox="0 0 24 24" fill="none"
            stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"
            aria-hidden="true" {
            (icon_paths(name))
        }
    }
}

fn icon_paths(name: &str) -> Markup {
    PreEscaped(match name {
        "panel-left" => r#"<rect width="18" height="18" x="3" y="3" rx="2"/><path d="M9 3v18"/>"#,
        "settings" => r#"<path d="M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.38a2 2 0 0 0-.73-2.73l-.15-.09a2 2 0 0 1-1-1.74v-.51a2 2 0 0 1 1-1.72l.15-.1a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z"/><circle cx="12" cy="12" r="3"/>"#,
        "square-pen" => r#"<path d="M12 3H5a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-7"/><path d="M18.375 2.625a2.121 2.121 0 1 1 3 3L12 15l-4 1 1-4Z"/>"#,
        "search" => r#"<circle cx="11" cy="11" r="8"/><path d="m21 21-4.3-4.3"/>"#,
        "x" => r#"<path d="M18 6 6 18"/><path d="m6 6 12 12"/>"#,
        "plus" => r#"<path d="M5 12h14"/><path d="M12 5v14"/>"#,
        "arrow-up" => r#"<path d="m5 12 7-7 7 7"/><path d="M12 19V5"/>"#,
        "box" => r#"<path d="m21 8-9-5-9 5 9 5 9-5Z"/><path d="M3 8v8l9 5 9-5V8"/><path d="M12 13v8"/>"#,
        "rotate-ccw" => r#"<path d="M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8"/><path d="M3 3v5h5"/>"#,
        "save" => r#"<path d="M15.2 3a2 2 0 0 1 1.4.6l3.8 3.8a2 2 0 0 1 .6 1.4V19a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2z"/><path d="M17 21v-7H7v7"/><path d="M7 3v5h8"/>"#,
        "refresh-cw" => r#"<path d="M3 12a9 9 0 0 1 15-6.7L21 8"/><path d="M21 3v5h-5"/><path d="M21 12a9 9 0 0 1-15 6.7L3 16"/><path d="M3 21v-5h5"/>"#,
        "database" => r#"<ellipse cx="12" cy="5" rx="9" ry="3"/><path d="M3 5v14c0 1.7 4 3 9 3s9-1.3 9-3V5"/><path d="M3 12c0 1.7 4 3 9 3s9-1.3 9-3"/>"#,
        "download" => r#"<path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><path d="M7 10l5 5 5-5"/><path d="M12 15V3"/>"#,
        "copy" => r#"<rect width="14" height="14" x="8" y="8" rx="2"/><path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2"/>"#,
        "snowflake" => r#"<path d="M2 12h20"/><path d="M12 2v20"/><path d="m20 16-4-4 4-4"/><path d="m4 8 4 4-4 4"/><path d="m16 4-4 4-4-4"/><path d="m8 20 4-4 4 4"/>"#,
        "circle-ellipsis" => r#"<circle cx="12" cy="12" r="10"/><path d="M8 12h.01"/><path d="M12 12h.01"/><path d="M16 12h.01"/>"#,
        "trash-2" => r#"<path d="M3 6h18"/><path d="M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/><path d="M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6"/><path d="M10 11v6"/><path d="M14 11v6"/>"#,
        "sliders" => r#"<path d="M4 21v-7"/><path d="M4 10V3"/><path d="M12 21v-9"/><path d="M12 8V3"/><path d="M20 21v-5"/><path d="M20 12V3"/><path d="M2 14h4"/><path d="M10 8h4"/><path d="M18 16h4"/>"#,
        "sliders-horizontal" => r#"<line x1="21" x2="14" y1="4" y2="4"/><line x1="10" x2="3" y1="4" y2="4"/><line x1="21" x2="12" y1="12" y2="12"/><line x1="8" x2="3" y1="12" y2="12"/><line x1="21" x2="16" y1="20" y2="20"/><line x1="12" x2="3" y1="20" y2="20"/><line x1="14" x2="14" y1="2" y2="6"/><line x1="8" x2="8" y1="10" y2="14"/><line x1="16" x2="16" y1="18" y2="22"/>"#,
        "monitor" => r#"<rect width="20" height="14" x="2" y="3" rx="2"/><path d="M8 21h8"/><path d="M12 17v4"/>"#,
        "funnel" => r#"<path d="M10 20a1 1 0 0 0 .55.9l2 1A1 1 0 0 0 14 21v-7a2 2 0 0 1 .6-1.4L21 6.2A2 2 0 0 0 19.6 3H4.4A2 2 0 0 0 3 6.2l6.4 6.4A2 2 0 0 1 10 14z"/>"#,
        "alert-triangle" => r#"<path d="m21.73 18-8-14a2 2 0 0 0-3.46 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3"/><path d="M12 9v4"/><path d="M12 17h.01"/>"#,
        "list-restart" => r#"<path d="M21 6H3"/><path d="M7 12H3"/><path d="M7 18H3"/><path d="M12 18a5 5 0 1 0-5-5"/><path d="M7 8v5h5"/>"#,
        "pencil-ruler" => r#"<path d="M13 7 8.7 2.7a2.4 2.4 0 0 0-3.4 0L2.7 5.3a2.4 2.4 0 0 0 0 3.4L7 13"/><path d="m8 6 2-2"/><path d="m18 16 2-2"/><path d="m17 11 4 4-6 6-4-4Z"/><path d="M14 14 4 4"/>"#,
        "pencil" => r#"<path d="M21.17 6.812a3 3 0 0 0-4.24-4.24L3 16.5V21h4.5Z"/><path d="m15 5 4 4"/>"#,
        "folder-open" => r#"<path d="m6 14 1.5-2.9A2 2 0 0 1 9.2 10H20a2 2 0 0 1 1.8 2.9l-2 4A2 2 0 0 1 18 18H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h3.9a2 2 0 0 1 1.7.9l.8 1.2A2 2 0 0 0 13.1 6H19a2 2 0 0 1 2 2v2"/>"#,
        "code" => r#"<path d="m16 18 6-6-6-6"/><path d="m8 6-6 6 6 6"/>"#,
        "square" => r#"<rect width="14" height="14" x="5" y="5" rx="2"/>"#,
        "skip-forward" => r#"<path d="m13 19 9-7-9-7v14Z"/><path d="M2 19V5"/>"#,
        "users" => r#"<path d="M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2"/><circle cx="9" cy="7" r="4"/><path d="M22 21v-2a4 4 0 0 0-3-3.87"/><path d="M16 3.13a4 4 0 0 1 0 7.75"/>"#,
        "user-round" => r#"<circle cx="12" cy="8" r="5"/><path d="M20 21a8 8 0 0 0-16 0"/>"#,
        "message-square" => r#"<path d="M21 15a4 4 0 0 1-4 4H7l-4 4V7a4 4 0 0 1 4-4h10a4 4 0 0 1 4 4z"/>"#,
        "message-circle" => r#"<path d="M7.9 20A9 9 0 1 0 4 16.1L2 22Z"/>"#,
        "sparkles" => r#"<path d="m12 3-1.9 5.1L5 10l5.1 1.9L12 17l1.9-5.1L19 10l-5.1-1.9Z"/><path d="M5 3v4"/><path d="M3 5h4"/><path d="M19 17v4"/><path d="M17 19h4"/>"#,
        "loader-circle" => r#"<path d="M21 12a9 9 0 1 1-6.219-8.56"/>"#,
        "check" => r#"<path d="M20 6 9 17l-5-5"/>"#,
        "wrench" => r#"<path d="M14.7 6.3a4 4 0 0 0-5-5L7 4l3 3 2.7-2.7a4 4 0 0 0 5 5L9.4 17.6a2 2 0 1 1-3-3Z"/>"#,
        "image" => r#"<rect width="18" height="18" x="3" y="3" rx="2"/><circle cx="9" cy="9" r="2"/><path d="m21 15-3.1-3.1a2 2 0 0 0-2.8 0L6 21"/>"#,
        "audio-lines" => r#"<path d="M2 10v3"/><path d="M6 6v11"/><path d="M10 3v18"/><path d="M14 8v7"/><path d="M18 5v13"/><path d="M22 10v3"/>"#,
        "file-text" => r#"<path d="M14.5 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7.5Z"/><polyline points="14 2 14 8 20 8"/><path d="M8 13h8"/><path d="M8 17h8"/>"#,
        "file" => r#"<path d="M14.5 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7.5Z"/><polyline points="14 2 14 8 20 8"/>"#,
        "film" => r#"<rect width="18" height="18" x="3" y="3" rx="2"/><path d="M7 3v18"/><path d="M3 7.5h4"/><path d="M3 12h18"/><path d="M3 16.5h4"/><path d="M17 3v18"/><path d="M17 7.5h4"/><path d="M17 16.5h4"/>"#,
        "chevron-left" => r#"<path d="m15 18-6-6 6-6"/>"#,
        "chevron-right" => r#"<path d="m9 18 6-6-6-6"/>"#,
        _ => "",
    }
    .to_string())
}

fn control(key: &str) -> &'static ControlSpec {
    CONTROL_SPECS
        .iter()
        .find(|control| control.affordance == key || control.command == key)
        .unwrap_or_else(|| panic!("missing control spec for {key}"))
}

fn markdown_content(content: &str) -> Markup {
    let mut blocks = Vec::new();
    let mut in_code = false;
    let mut code_lang = String::new();
    let mut code = Vec::new();
    let mut paragraph = Vec::new();
    let mut list = Vec::new();
    let mut table = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim_end();
        if trimmed.starts_with("```") {
            if in_code {
                blocks.push(markdown_code_block(&code_lang, &code.join("\n")));
                code.clear();
                code_lang.clear();
                in_code = false;
            } else {
                flush_paragraph(&mut blocks, &mut paragraph);
                flush_list(&mut blocks, &mut list);
                flush_table(&mut blocks, &mut table);
                code_lang = trimmed.trim_start_matches("```").trim().to_string();
                in_code = true;
            }
            continue;
        }
        if in_code {
            code.push(trimmed.to_string());
            continue;
        }
        let plain = trimmed.trim();
        if plain.is_empty() {
            flush_paragraph(&mut blocks, &mut paragraph);
            flush_list(&mut blocks, &mut list);
            flush_table(&mut blocks, &mut table);
        } else if is_table_line(plain) {
            flush_paragraph(&mut blocks, &mut paragraph);
            flush_list(&mut blocks, &mut list);
            let cells = markdown_table_cells(plain);
            if !is_table_separator(&cells) {
                table.push(cells);
            }
        } else if let Some(heading) = plain.strip_prefix("### ") {
            flush_paragraph(&mut blocks, &mut paragraph);
            flush_list(&mut blocks, &mut list);
            flush_table(&mut blocks, &mut table);
            blocks.push(html! { h4 { (heading) } });
        } else if let Some(heading) = plain.strip_prefix("## ") {
            flush_paragraph(&mut blocks, &mut paragraph);
            flush_list(&mut blocks, &mut list);
            flush_table(&mut blocks, &mut table);
            blocks.push(html! { h3 { (heading) } });
        } else if let Some(heading) = plain.strip_prefix("# ") {
            flush_paragraph(&mut blocks, &mut paragraph);
            flush_list(&mut blocks, &mut list);
            flush_table(&mut blocks, &mut table);
            blocks.push(html! { h2 { (heading) } });
        } else if let Some(quote) = plain.strip_prefix("> ") {
            flush_paragraph(&mut blocks, &mut paragraph);
            flush_list(&mut blocks, &mut list);
            flush_table(&mut blocks, &mut table);
            blocks.push(html! { blockquote { (quote) } });
        } else if let Some(item) = plain
            .strip_prefix("- ")
            .or_else(|| plain.strip_prefix("* "))
        {
            flush_paragraph(&mut blocks, &mut paragraph);
            flush_table(&mut blocks, &mut table);
            list.push(item.to_string());
        } else {
            flush_list(&mut blocks, &mut list);
            flush_table(&mut blocks, &mut table);
            paragraph.push(plain.to_string());
        }
    }
    if in_code {
        blocks.push(markdown_code_block(&code_lang, &code.join("\n")));
    }
    flush_paragraph(&mut blocks, &mut paragraph);
    flush_list(&mut blocks, &mut list);
    flush_table(&mut blocks, &mut table);
    html! { div class="markdown-content" { @for block in blocks { (block) } } }
}

fn markdown_code_block(language: &str, code: &str) -> Markup {
    html! {
        figure class="code-block" {
            @if !language.is_empty() {
                figcaption { (language) }
            }
            pre { code { (code) } }
        }
    }
}

fn flush_paragraph(blocks: &mut Vec<Markup>, paragraph: &mut Vec<String>) {
    if paragraph.is_empty() {
        return;
    }
    let text = paragraph.join("\n");
    blocks.push(html! { p { (text) } });
    paragraph.clear();
}

fn flush_list(blocks: &mut Vec<Markup>, list: &mut Vec<String>) {
    if list.is_empty() {
        return;
    }
    blocks.push(html! { ul { @for item in list.iter() { li { (item) } } } });
    list.clear();
}

fn flush_table(blocks: &mut Vec<Markup>, table: &mut Vec<Vec<String>>) {
    if table.is_empty() {
        return;
    }
    let headers = table.first().cloned().unwrap_or_default();
    let rows = if table.len() > 1 { &table[1..] } else { &[] };
    blocks.push(html! {
        table class="markdown-table" {
            thead {
                tr {
                    @for header in &headers {
                        th { (header) }
                    }
                }
            }
            tbody {
                @for row in rows {
                    tr {
                        @for cell in row {
                            td { (cell) }
                        }
                    }
                }
            }
        }
    });
    table.clear();
}

fn is_table_line(line: &str) -> bool {
    line.starts_with('|') && line.ends_with('|') && line.matches('|').count() >= 2
}

fn markdown_table_cells(line: &str) -> Vec<String> {
    line.trim_matches('|')
        .split('|')
        .map(|cell| cell.trim().to_string())
        .collect()
}

fn is_table_separator(cells: &[String]) -> bool {
    !cells.is_empty()
        && cells.iter().all(|cell| {
            let cell = cell.trim();
            !cell.is_empty()
                && cell.chars().all(|ch| matches!(ch, '-' | ':' | ' '))
                && cell.chars().any(|ch| ch == '-')
        })
}

fn active_conversation(
    conversations: &CommandResult<Vec<Conversation>>,
    selected_id: Option<&str>,
) -> Option<Conversation> {
    let items = conversations.result.as_ref()?;
    selected_id
        .and_then(|id| items.iter().find(|conversation| conversation.id == id))
        .or_else(|| {
            items
                .iter()
                .find(|conversation| conversation.kind == ConversationKind::Chat)
        })
        .map(mom_llama_runtime::conversation_store::project_conversation)
}

fn role_label(role: &MessageRole) -> &'static str {
    match role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
    }
}

fn readiness_short_label(engine: &CommandResult<impl Serialize>) -> String {
    if engine.status == "host_integrated" {
        "Model loaded".to_string()
    } else if engine.status == "configured" {
        "Ready to load".to_string()
    } else {
        engine
            .blocker
            .as_ref()
            .map(|blocker| blocker.message.clone())
            .unwrap_or_else(|| "Needs setup".to_string())
    }
}

fn model_label(settings: &CommandResult<Settings>) -> String {
    settings
        .result
        .as_ref()
        .and_then(|settings| settings.model_path.as_ref())
        .map(|path| file_name(path))
        .unwrap_or_else(|| "No model".to_string())
}

fn model_chip_label(settings: &CommandResult<Settings>) -> String {
    let label = model_label(settings);
    let raw = upstream_settings_bool(settings, "showRawModelNames");
    let mut display = if raw {
        label.trim_end_matches(".gguf").to_string()
    } else {
        label
            .trim_end_matches(".gguf")
            .split(['-', '_'])
            .take(2)
            .collect::<Vec<_>>()
            .join("-")
    };
    if upstream_settings_bool(settings, "showModelQuantization")
        && let Some(quantization) = model_quantization(&label)
    {
        display.push_str(&format!(" · {quantization}"));
    }
    if upstream_settings_bool(settings, "showModelTags") {
        let tags = model_tags(settings, &label);
        if !tags.is_empty() {
            display.push_str(&format!(" · {}", tags.join(" · ")));
        }
    }
    display
}

fn model_quantization(label: &str) -> Option<String> {
    label
        .trim_end_matches(".gguf")
        .split(['-', '_', '.'])
        .rev()
        .find(|part| {
            let upper = part.to_ascii_uppercase();
            upper.starts_with('Q') && upper.chars().nth(1).is_some_and(|ch| ch.is_ascii_digit())
                || matches!(upper.as_str(), "F16" | "F32" | "BF16")
        })
        .map(|part| part.to_ascii_uppercase())
}

fn model_tags(settings: &CommandResult<Settings>, label: &str) -> Vec<&'static str> {
    let mut tags = vec!["local"];
    if settings
        .result
        .as_ref()
        .and_then(|settings| settings.mmproj_path.as_ref())
        .is_some()
    {
        tags.push("multimodal");
    }
    let lower = label.to_ascii_lowercase();
    if ["qwen3", "deepseek", "reasoning", "r1"]
        .iter()
        .any(|needle| lower.contains(needle))
    {
        tags.push("reasoning");
    }
    tags
}

fn models_available_for_label(settings: &CommandResult<Settings>) -> bool {
    settings
        .result
        .as_ref()
        .and_then(|settings| settings.model_path.as_ref())
        .is_some()
}

fn model_path(settings: &CommandResult<Settings>) -> String {
    settings
        .result
        .as_ref()
        .and_then(|settings| settings.model_path.as_ref())
        .map(|path| path.display().to_string())
        .unwrap_or_default()
}

fn settings_value(settings: &CommandResult<Settings>, key: &str) -> String {
    let Some(settings) = settings.result.as_ref() else {
        return String::new();
    };
    match key {
        "mmproj_path" => settings
            .mmproj_path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_default(),
        "native_device" => format!("{:?}", settings.native_device).to_lowercase(),
        "context_tokens" => settings.context_tokens.to_string(),
        "batch_tokens" => settings.batch_tokens.to_string(),
        "max_parallel_sequences" => settings.max_parallel_sequences.to_string(),
        "memory_budget_mib" => (settings.resident_memory_budget_bytes / (1024 * 1024)).to_string(),
        "model_path" => settings
            .model_path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_default(),
        "default_temperature" => settings.default_temperature.to_string(),
        "default_top_p" => settings.default_top_p.to_string(),
        "default_max_tokens" => settings.default_max_tokens.to_string(),
        "kv_cache_policy" => match settings.kv_cache_policy {
            KvCachePolicy::None => "none",
            KvCachePolicy::PromptPrefix => "prompt_prefix",
            KvCachePolicy::KvCacheCandidate => "kv_cache_candidate",
        }
        .to_string(),
        _ => String::new(),
    }
}

fn kv_policy_value(policy: &KvCachePolicy) -> &'static str {
    match policy {
        KvCachePolicy::None => "none",
        KvCachePolicy::PromptPrefix => "prompt_prefix",
        KvCachePolicy::KvCacheCandidate => "kv_cache_candidate",
    }
}

fn upstream_settings_value(settings: &CommandResult<Settings>, key: &str) -> String {
    let Some(settings) = settings.result.as_ref() else {
        return String::new();
    };
    match key {
        "temperature" => settings.default_temperature.to_string(),
        "top_p" => settings.default_top_p.to_string(),
        "max_tokens" => settings.default_max_tokens.to_string(),
        _ => settings
            .upstream_settings
            .get(key)
            .map(json_value_to_form_value)
            .unwrap_or_default(),
    }
}

fn upstream_settings_bool(settings: &CommandResult<Settings>, key: &str) -> bool {
    settings
        .result
        .as_ref()
        .and_then(|settings| settings.upstream_settings.get(key))
        .and_then(|value| match value {
            Value::Bool(value) => Some(*value),
            Value::String(value) => value.parse::<bool>().ok(),
            _ => None,
        })
        .unwrap_or(false)
}

fn json_value_to_form_value(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(value) => value.clone(),
        Value::Number(value) => value.to_string(),
        Value::Bool(value) => value.to_string(),
        value => value.to_string(),
    }
}

fn kv_label(kv: &CommandResult<KvCacheStatus>) -> String {
    let Some(status) = kv.result.as_ref() else {
        return "Cache unavailable".to_string();
    };
    match status.status {
        mom_llama_runtime::kv_cache::KvCacheState::Disabled => "Caching off".to_string(),
        mom_llama_runtime::kv_cache::KvCacheState::UnsupportedByEngine => {
            "Cache unsupported".to_string()
        }
        mom_llama_runtime::kv_cache::KvCacheState::BlockedMissingModel => "No model".to_string(),
        mom_llama_runtime::kv_cache::KvCacheState::BlockedMissingCacheDir => {
            "Cache not initialized".to_string()
        }
        mom_llama_runtime::kv_cache::KvCacheState::ConfiguredNotVerified => {
            "Cache unverified".to_string()
        }
        mom_llama_runtime::kv_cache::KvCacheState::PromptSmokeVerified => {
            "Cache smoke verified".to_string()
        }
        mom_llama_runtime::kv_cache::KvCacheState::Saved => "Cache saved".to_string(),
        mom_llama_runtime::kv_cache::KvCacheState::Restored => "Cache restored".to_string(),
        mom_llama_runtime::kv_cache::KvCacheState::Invalidated => "Cache cleared".to_string(),
    }
}

fn message_count(conversation: &Conversation) -> String {
    match conversation.messages.len() {
        0 => "No messages".to_string(),
        1 => "1 message".to_string(),
        count => format!("{count} messages"),
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_else(|| path.to_str().unwrap_or("model.gguf"))
        .to_string()
}

fn human_bytes(size: Option<u64>) -> String {
    let Some(size) = size else {
        return "unknown size".to_string();
    };
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    if size >= 1024 * 1024 * 1024 {
        format!("{:.1} GiB", size as f64 / GIB)
    } else if size >= 1024 * 1024 {
        format!("{:.1} MiB", size as f64 / MIB)
    } else {
        format!("{size} bytes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static VIEW_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn rendered_app_controls_have_contract_metadata() -> Result<()> {
        let _guard = VIEW_TEST_LOCK.lock().expect("lock view test state");
        let data_dir = std::env::temp_dir().join(format!(
            "mom-llama-view-test-{}",
            mom_llama_runtime::now_ms()
        ));
        mom_llama_runtime::config::set_data_dir_override_for_tests(Some(data_dir));
        let html = render_app()?;
        mom_llama_runtime::config::set_data_dir_override_for_tests(None);
        for forbidden in ["__sveltekit__", "React", "Vue", "fetch("] {
            assert!(
                !html.contains(forbidden),
                "found forbidden marker {forbidden}"
            );
        }
        for tag in ["button", "input", "textarea", "select", "form"] {
            assert_interactive_tags_have_metadata(&html, tag);
        }
        assert_buttons_use_approved_components(&html);
        assert_eq!(
            html.matches("<textarea ").count(),
            html.matches("</textarea>").count(),
            "every rendered textarea must have an explicit closing tag"
        );
        assert!(
            !html.contains("<a "),
            "native shell should not expose unmanaged anchors"
        );
        assert!(
            !html.contains("<summary"),
            "native shell should not expose unmanaged disclosure controls"
        );
        assert!(
            html.contains(
                r#"id="settings-modal" class="modal-backdrop is-hidden" hidden aria-hidden="true""#
            ),
            "settings modal must be hidden until its Rust-owned command affordance opens it"
        );
        assert!(html.contains("Current chat instructions"));
        assert!(html.contains(r#"data-chat-setting="system_message""#));
        assert!(html.contains("Changes save automatically"));
        assert!(!html.contains("Save settings"));
        assert!(html.contains(r#"class="settings-autosave" data-state="idle""#));
        assert!(html.contains("settings-save-glyph-saved"));
        assert!(html.contains("settings-save-glyph-saving"));
        assert!(html.contains(r#"name="kv_cache_policy""#));
        assert!(html.contains("Automatic (recommended)"));
        assert!(html.contains("Prefixes only"));
        assert!(html.contains(">Off</option>"));
        assert!(html.contains("Off neither creates nor reads cached prompt state."));
        assert!(!html.contains("Pre-fill KV cache after response"));
        assert!(
            html.contains(
                r#"id="command-output" class="sr-command-output" aria-hidden="true" tabindex="-1""#
            ),
            "machine-readable command receipts must not be announced as raw JSON"
        );
        assert!(
            html.contains(
                r#"id="tool-approval-modal" class="modal-backdrop is-hidden" hidden aria-hidden="true""#
            ),
            "tool authority must stay behind an explicit hidden approval dialog"
        );
        assert!(html.contains(r#"id="consult-view" class="consult-picker is-hidden""#));
        assert!(html.contains(
            r#"id="persona-view" class="persona-picker is-hidden" aria-label="Personas" hidden aria-hidden="true""#
        ));
        assert!(html.contains(r#"data-action="personas-open""#));
        assert!(html.contains("Open a saved Persona or start a new chat."));
        assert!(html.contains("Bessel van der Kolk"));
        assert!(!html.contains("Body &amp; trauma lens"));
        assert!(!html.contains(r#"data-action="skills-open""#));
        assert!(html.contains(r#"data-action="persona-open""#));
        assert!(!html.contains("Edits version this template"));
        assert!(!html.contains(r#"class="persona-template-banner""#));
        assert!(html.contains(r#"id="mention-candidates" class="mention-candidates is-hidden""#));
        let stylesheet = include_str!("../../ui/style.css");
        assert!(
            stylesheet.contains(".composer {\n  position: relative;"),
            "mention autocomplete must be positioned relative to the composer"
        );
        assert!(
            stylesheet.contains("bottom: calc(100% + 10px);"),
            "mention autocomplete must open above the bottom composer"
        );
        assert!(
            stylesheet.contains(".message-actions {\n  display: flex;")
                && stylesheet.contains("opacity: 0;")
                && stylesheet.contains(".message-row:hover > .message-actions,")
                && stylesheet.contains(".message-row:focus-within > .message-actions,"),
            "message actions must stay latent until hover or keyboard focus"
        );
        for forbidden_consult_surface in [
            r#"class="consult-grid""#,
            r#"class="consult-seat""#,
            r#"id="consult-form""#,
            "This perspective will appear here.",
        ] {
            assert!(
                !html.contains(forbidden_consult_surface),
                "legacy Consult dashboard leaked into the conversation surface: {forbidden_consult_surface}"
            );
        }
        assert!(
            html.contains(r#"id="conversation-search-form" class="nav-search is-hidden""#),
            "search must be an in-app command-backed form"
        );
        assert!(
            !html.contains("conversation-search-prompt"),
            "search must not fall back to prompt-only behavior"
        );
        for section in [
            "general",
            "display",
            "personas",
            "consult",
            "sampling",
            "penalties",
            "agentic",
            "tools",
            "mcp",
            "import-export",
            "developer",
        ] {
            assert!(
                html.contains(&format!(r#"data-section="{section}""#)),
                "missing settings section {section}"
            );
        }
        assert!(
            html.contains(r#"data-section-panel="personas""#)
                && html.contains(r#"id="persona-editor" class="settings-card persona-editor is-hidden""#)
                && html.contains(r#"data-section-panel="consult""#)
                && html.contains(r#"id="persona-group-editor" class="settings-card persona-group-editor is-hidden""#),
            "persona definitions and group patterns must live in their Settings sections"
        );
        assert!(html.contains("lucide-search"));
        assert!(html.contains("mcp-logo"));
        let js = include_str!("../../ui/coop-hx.js");
        assert!(
            js.contains("invokeMarkup") && js.contains("Uint8Array.from(response)"),
            "large Rust-rendered fragments must use the raw IPC decoder"
        );
        assert!(js.contains("renderSearchResults"));
        assert!(
            js.contains("captureChatViewport")
                && js.contains("restoreChatViewport")
                && js.contains("state.followTail")
                && js.contains("keepLiveTailVisible")
                && js.contains("stream.dataset.followTail"),
            "chat fragment refreshes must preserve the reader's transcript position"
        );
        assert!(js.contains("mom_llama_render_persona_picker_fragment"));
        assert!(js.contains("selectedConversationKind"));
        assert!(js.contains(r#"selectedConversationKind() === "persona_template""#));
        assert!(js.contains("const instantiated = await instantiatePersona(sourceConversation)"));
        assert!(js.contains("renderConsultGroups"));
        assert!(js.contains("refreshConsult()"));
        assert!(js.contains("scheduleSettingsAutosave"));
        assert!(js.contains("mom_llama_conversation_system_message_update"));
        assert!(js.contains("Couldn’t save changes"));
        assert!(js.contains("autosave.dataset.state = \"idle\""));
        assert!(
            stylesheet
                .contains(".settings-autosave[data-state=\"saved\"] .settings-save-glyph-saved")
                && stylesheet.contains("@keyframes settings-save-spin"),
            "autosave feedback must stay icon-only, transient, and native to the established icon language"
        );
        assert!(js.contains(r#"openSettings("consult")"#));
        let composer_clear = js
            .find(r#"if (textarea) textarea.value = "";"#)
            .expect("composer must clear optimistically when a send is accepted");
        let dispatch = js
            .find(r#"result = await invoke("mom_llama_chat_dispatch"#)
            .expect("composer must dispatch through the canonical Rust command");
        assert!(
            composer_clear < dispatch,
            "the composer must clear before awaiting model generation"
        );
        let chat_submit_start = js
            .find(r#"if (form.id === "chat-form")"#)
            .expect("chat submit handler must exist");
        let chat_submit_end = js[chat_submit_start..]
            .find(r#"if (form.id === "settings-form")"#)
            .map(|offset| chat_submit_start + offset)
            .expect("chat submit handler must be bounded");
        let chat_submit = &js[chat_submit_start..chat_submit_end];
        let dispatch_gate = chat_submit
            .find(r#"const dispatchLease = acquireChatBusy("chat-dispatch");"#)
            .expect("chat submit must acquire its dispatch gate");
        let first_await = chat_submit
            .find("await ")
            .expect("chat submit must perform asynchronous work");
        assert!(
            dispatch_gate < first_await,
            "chat submit must acquire the busy gate before persona, draft, or generation awaits"
        );
        let attachment_collection = js
            .find("const attachmentIds = draftAttachmentIds(form);")
            .expect("composer must collect staged attachments");
        let empty_send_guard = js
            .find("if (!message && !attachmentIds.length) return;")
            .expect("composer must accept attachment-only sends");
        assert!(
            attachment_collection < empty_send_guard
                && js.contains("if (message) appendLiveMessage(\"user\", message"),
            "attachment-only sends must dispatch without an empty optimistic message bubble"
        );
        let persist_draft_start = js
            .find("const persistDraftNow = async")
            .expect("the immediate draft persistence helper must exist");
        let persist_draft_end = js[persist_draft_start..]
            .find("const scheduleDraft")
            .map(|offset| persist_draft_start + offset)
            .expect("the immediate draft persistence helper must be bounded");
        let persist_draft = &js[persist_draft_start..persist_draft_end];
        assert!(
            js.contains("const draftAttachmentIds")
                && persist_draft.contains("attachmentIds: [...attachmentIds]")
                && !persist_draft.contains("attachmentIds: []"),
            "draft persistence must preserve the Rust-owned staged attachment set"
        );
        assert!(
            !js.contains("mom_llama_draft_clear"),
            "the view must never clear a draft before Rust commits a successful generation"
        );
        assert!(
            js.contains(r#""draft-attachment-remove": async (button)"#)
                && js.contains("await persistDraftNow(message, attachmentIds)"),
            "attachment removal and blocked sends must write an explicit attachment-id set"
        );
        let picker = include_str!("commands.rs");
        for extension in ["docx", "odt", "epub", "avif", "aiff", "webm", "zip", "7z"] {
            assert!(
                picker.contains(&format!(r#""{extension}""#)),
                "native attachment picker is missing {extension}"
            );
        }
        assert!(js.contains("mom_llama_tool_loop_prepare"));
        assert!(js.contains("approvalId: modal.dataset.approvalId"));
        assert!(js.contains("mom_llama_tool_loop_cancel"));
        assert!(js.contains("mom_llama_tool_loop_stream"));
        assert!(js.contains("tool_call_started"));
        assert!(js.contains("tool_result"));
        assert!(js.contains("model_delta"));
        assert!(
            !js.contains("value?.readiness || value?.status"),
            "normal success toasts must not expose scaffold readiness jargon"
        );
        assert!(
            js.contains("mom_llama_attachment_preview_bytes")
                && js.contains("response instanceof ArrayBuffer")
                && js.contains(
                    "Attachment preview returned serialized JSON instead of raw IPC bytes."
                ),
            "attachment payloads must cross Tauri IPC as raw bounded bytes, never JSON arrays"
        );
        assert!(
            js.contains("const ATTACHMENT_PREVIEW_CONCURRENCY = 2;")
                && js.contains("const ATTACHMENT_PREVIEW_LIVE_LIMIT = 4;")
                && js.contains("new IntersectionObserver")
                && js.contains("URL.revokeObjectURL(entry.url)")
                && js.contains("releaseAttachmentObjectUrls(current)"),
            "attachment previews must hydrate lazily with bounded concurrency and object-URL lifetime"
        );
        let commands = include_str!("commands.rs");
        assert!(
            commands.contains("const MAX_ATTACHMENT_PREVIEW_BYTES: u64 = 16 * 1024 * 1024;")
                && commands.contains("attachment_preview(&attachment, false)")
                && commands.contains("Ok(Response::new(bytes))"),
            "the native preview path must check metadata before returning a raw bounded IPC body"
        );
        assert!(
            js.contains("result?.blocker?.code !== \"no_active_tool_loop\""),
            "tool-loop stop must bridge the approval-to-registration race"
        );
        assert!(!js.contains("window.confirm"));
        assert!(
            !js.contains("conversation-search-prompt"),
            "search JS must not use the removed prompt-only action"
        );
        assert!(
            !js.contains("Search conversations\", \"\""),
            "search must not use window.prompt"
        );
        let css = include_str!("../../ui/style.css");
        assert!(
            !css.contains("nav-button::before"),
            "navigation icons must be rendered from upstream-style SVGs"
        );
        let tauri_config = include_str!("../tauri.conf.json");
        assert!(
            tauri_config.contains("img-src 'self' asset: data: blob:")
                && tauri_config.contains("media-src 'self' asset: data: blob:"),
            "the packaged CSP must permit only local blob-backed attachment media"
        );
        Ok(())
    }

    #[test]
    fn compact_layout_is_reachable_at_the_native_window_minimum() -> Result<()> {
        let config: Value = serde_json::from_str(include_str!("../tauri.conf.json"))?;
        assert_eq!(
            config
                .pointer("/app/windows/0/minWidth")
                .and_then(Value::as_u64),
            Some(760),
            "the native minimum width must enter the 900px compact layout"
        );
        assert!(
            include_str!("../../ui/style.css").contains("@media (max-width: 900px)"),
            "the compact breakpoint must remain paired with the native window minimum"
        );
        Ok(())
    }

    #[test]
    fn selected_persona_projects_its_normal_editable_transcript() {
        let persona_message =
            test_message("persona-answer", MessageRole::Assistant, "Template reply");
        let chat = test_conversation("chat", "Chat", ConversationKind::Chat, Vec::new());
        let mut persona = test_conversation(
            "persona",
            "Persona",
            ConversationKind::PersonaTemplate,
            vec![persona_message],
        );
        persona.execution_profile.mention_handle = "persona".to_string();
        let conversations = CommandResult::passed(
            "mom_llama.conversation_list",
            "contracted",
            vec![chat, persona],
            Vec::new(),
            Vec::new(),
            false,
            false,
        );
        let active = active_conversation(&conversations, Some("persona"))
            .expect("selected persona must project");
        assert_eq!(active.kind, ConversationKind::PersonaTemplate);
        assert_eq!(active.messages.len(), 1);

        let actions = message_actions(&active.messages[0], false, false, true, false).into_string();
        assert!(actions.contains(r#"data-action="message-edit""#));
        assert!(actions.contains(r#"data-action="message-delete""#));
        assert!(!actions.contains(r#"data-action="persona-freeze""#));
        assert!(!actions.contains(r#"data-action="chat-regenerate""#));
        assert!(!actions.contains(r#"data-action="chat-continue""#));

        let context = persona_context(&active).into_string();
        assert!(context.contains(r#"class="persona-context""#));
        assert!(context.contains(r#"data-action="persona-profile-open""#));
        assert!(context.contains(r#"data-action="persona-instantiate""#));
        assert!(!context.contains("Sending starts a separate chat"));
        assert!(!context.contains("Edits version this template"));
    }

    #[test]
    fn generation_controls_are_reserved_for_the_active_ordinary_assistant() {
        let assistant = test_message("assistant", MessageRole::Assistant, "Answer");
        let inactive = message_actions(&assistant, true, false, true, false).into_string();
        assert!(!inactive.contains(r#"data-action="chat-regenerate""#));
        assert!(!inactive.contains(r#"data-action="chat-continue""#));

        let active = message_actions(&assistant, true, true, true, false).into_string();
        assert!(active.contains(r#"data-action="chat-regenerate""#));
        assert!(active.contains(r#"data-action="chat-continue""#));
    }

    #[test]
    fn store_blockers_remain_typed_in_the_visible_projection() {
        let blocker = Blocker::new(
            "persona_store_unavailable",
            "Saved Personas could not be loaded from local storage.",
            Vec::new(),
        );
        let html = store_blocker(&blocker).into_string();
        assert!(html.contains(r#"data-blocker-code="persona_store_unavailable""#));
        assert!(html.contains("Saved Personas could not be loaded"));
    }

    #[test]
    fn corrupt_product_storage_never_projects_an_empty_default_chat() {
        let _guard = VIEW_TEST_LOCK.lock().expect("lock view test state");
        let data_dir = std::env::temp_dir().join(format!(
            "mom-llama-corrupt-view-test-{}-{}",
            std::process::id(),
            mom_llama_runtime::now_ms()
        ));
        std::fs::create_dir_all(&data_dir).expect("create corrupt store fixture");
        std::fs::write(data_dir.join("runtime.sqlite3"), b"not a sqlite database")
            .expect("write corrupt store fixture");
        mom_llama_runtime::config::set_data_dir_override_for_tests(Some(data_dir.clone()));
        let rendered = render_app();
        mom_llama_runtime::config::set_data_dir_override_for_tests(None);
        std::fs::remove_dir_all(data_dir).expect("remove corrupt store fixture");
        assert!(
            rendered.is_err(),
            "storage failure must reach the typed Tauri error path rather than rendering an empty chat"
        );
    }

    #[test]
    fn markdown_tables_render_without_frontend_runtime() {
        let html =
            markdown_content("| Name | Value |\n| --- | --- |\n| Cache | Ready |").into_string();
        assert!(html.contains(r#"<table class="markdown-table">"#));
        assert!(html.contains("<th>Name</th>"));
        assert!(html.contains("<td>Ready</td>"));
    }

    #[test]
    fn structured_tool_results_render_as_a_readable_card() {
        let message = Message {
            id: "tool".to_string(),
            conversation_id: "conversation".to_string(),
            role: MessageRole::Tool,
            content: r#"{
                "turn":1,
                "server":"fixture",
                "tool":"echo",
                "arguments":{"value":"ready"},
                "result":{"content":[{"type":"text","text":"fixture tool result"}]},
                "result_sha256":"e5ae5123456789"
            }"#
            .to_string(),
            created_at: "1".to_string(),
            parent_id: None,
            model: None,
            receipt_id: None,
            prompt_tokens: Some(7),
            completion_tokens: Some(3),
            reasoning_content: None,
            reasoning_incomplete: false,
            branch_index: None,
            branch_count: None,
            attribution: None,
            attachment_ids: Vec::new(),
        };
        let html = tool_message_content(&message, true, true).into_string();
        assert!(html.contains(r#"class="tool-result-card""#));
        assert!(!html.contains("tool-result-unstructured"));
        assert!(html.contains("<strong>echo</strong>"));
        assert!(html.contains("fixture · completed locally"));
        assert!(html.contains("fixture tool result"));
        assert!(html.contains("Arguments"));
        assert!(html.contains("Technical details"));
        assert!(html.contains("e5ae51234567"));
        assert!(html.contains("Turn 1 · 7 prompt tokens · 3 completion tokens"));
        assert!(!html.contains("<details"));
        assert!(!html.contains("<summary"));
        assert!(html.contains(r#"<section class="tool-result-details">"#));
        assert!(html.contains(r#"data-action="tool-details-toggle""#));
        assert!(html.contains(r#"aria-expanded="true""#));
        assert_interactive_tags_have_metadata(&html, "button");
    }

    #[test]
    fn assistant_reasoning_renders_separately_from_the_visible_answer() -> Result<()> {
        let settings = CommandResult::passed(
            "mom_llama.settings_get",
            "contracted",
            Settings::defaults_for_data_dir(std::env::temp_dir()),
            Vec::new(),
            Vec::new(),
            false,
            false,
        );
        let message = Message {
            id: "assistant".to_string(),
            conversation_id: "conversation".to_string(),
            role: MessageRole::Assistant,
            content: "Visible answer".to_string(),
            created_at: "1".to_string(),
            parent_id: None,
            model: Some("model.gguf".to_string()),
            receipt_id: None,
            prompt_tokens: Some(3),
            completion_tokens: Some(5),
            reasoning_content: Some("Private reasoning".to_string()),
            reasoning_incomplete: true,
            branch_index: None,
            branch_count: None,
            attribution: None,
            attachment_ids: Vec::new(),
        };
        let html = message_row(&message, &settings, &[], true, true).into_string();
        assert!(html.contains(r#"<section class="message-reasoning">"#));
        assert!(html.contains(r#"class="message-reasoning-label""#));
        assert!(!html.contains("<details"));
        assert!(!html.contains("<summary"));
        assert!(html.contains("Reasoning in progress"));
        assert!(html.contains("Private reasoning"));
        assert!(html.contains("Visible answer"));
        assert!(!html.contains("&lt;think&gt;"));
        Ok(())
    }

    #[test]
    fn dynamic_interactions_are_contract_backed_and_validate_stale_choices() {
        let js = include_str!("../../ui/coop-hx.js");
        for unmanaged in [
            r#"document.createElement("button")"#,
            r#"document.createElement("textarea")"#,
            r#"document.createElement("audio")"#,
            r#"document.createElement("details")"#,
            r#"document.createElement("summary")"#,
        ] {
            assert!(
                !js.contains(unmanaged),
                "dynamic interactive element bypasses createCommandElement: {unmanaged}"
            );
        }

        for (key, affordance) in [
            ("attachmentPreview", "attachment.preview"),
            ("conversationSelect", "conversation.select"),
            ("mentionCancel", "mention.cancel"),
            ("mentionCandidates", "mention.candidates"),
            ("personaGroupList", "persona_group.list"),
            ("messageEdit", "message.edit"),
        ] {
            assert_dynamic_js_contract(js, key, control(affordance));
        }

        for creation in [
            r#"createCommandElement("audio", DYNAMIC_CONTROL_SPECS.attachmentPreview)"#,
            r#"createCommandElement("button", DYNAMIC_CONTROL_SPECS.conversationSelect)"#,
            r#"createCommandElement("button", DYNAMIC_CONTROL_SPECS.mentionCancel)"#,
            r#"createCommandElement("button", DYNAMIC_CONTROL_SPECS.mentionCandidates)"#,
            r#"createCommandElement("button", DYNAMIC_CONTROL_SPECS.personaGroupList)"#,
            r#"createCommandElement("textarea", DYNAMIC_CONTROL_SPECS.messageEdit)"#,
            r#"createCommandElement("button", DYNAMIC_CONTROL_SPECS.messageEdit)"#,
        ] {
            assert!(
                js.contains(creation),
                "missing dynamic contract gate: {creation}"
            );
        }

        assert_command_precedes_local_mutation(
            js,
            r#""mention-insert": async (button)"#,
            r#"invoke("mom_llama_mention_candidates"#,
            "insertMention(textarea, handle)",
        );
        assert_command_precedes_local_mutation(
            js,
            r#""consult-group-insert": async (button)"#,
            r#"invoke("mom_llama_persona_group_list")"#,
            r#"textarea.value += `${prefix}@${handle} `"#,
        );
        assert_command_precedes_local_mutation(
            js,
            r#""tool-details-toggle": async (button)"#,
            r#"invoke("mom_llama_message_copy"#,
            r#"details.forEach((detail) => detail.classList.toggle"#,
        );
        assert!(
            js.contains(r#"if (settingEnabled("showThoughtInProgress")) reasoning?.classList.remove("is-hidden")"#),
            "live reasoning must honor the persisted display policy without an unmanaged disclosure"
        );
    }

    #[test]
    fn message_attachments_follow_the_messages_exact_order_and_keep_its_text() -> Result<()> {
        let settings = CommandResult::passed(
            "mom_llama.settings_get",
            "contracted",
            Settings::defaults_for_data_dir(std::env::temp_dir()),
            Vec::new(),
            Vec::new(),
            false,
            false,
        );
        let first = attachment_record("first", "message", "notes.md", AttachmentKind::Text);
        let second = attachment_record("second", "message", "photo.png", AttachmentKind::Image);
        let unrelated = attachment_record("unrelated", "other", "other.pdf", AttachmentKind::Pdf);
        let message = Message {
            id: "message".to_string(),
            conversation_id: "conversation".to_string(),
            role: MessageRole::User,
            content: "Please compare these.".to_string(),
            created_at: "1".to_string(),
            parent_id: None,
            model: None,
            receipt_id: None,
            prompt_tokens: None,
            completion_tokens: None,
            reasoning_content: None,
            reasoning_incomplete: false,
            branch_index: None,
            branch_count: None,
            attribution: None,
            attachment_ids: vec!["second".to_string(), "first".to_string()],
        };
        let attachments = vec![first.clone(), second.clone(), unrelated];
        let records = message_attachment_records(&message, &attachments);
        assert_eq!(
            records
                .iter()
                .map(|record| record.id.as_str())
                .collect::<Vec<_>>(),
            ["second", "first"]
        );
        let html = message_row(&message, &settings, &records, true, false).into_string();
        let second_position = html.find("photo.png").expect("second attachment");
        let first_position = html.find("notes.md").expect("first attachment");
        assert!(second_position < first_position);
        assert!(html.contains("Please compare these."));
        assert!(!html.contains("other.pdf"));
        Ok(())
    }

    #[test]
    fn composer_renders_staged_draft_attachments_as_compact_removable_chips() {
        let engine = CommandResult::passed(
            "mom_llama.engine_check",
            "host_integrated",
            serde_json::json!({"ready": true}),
            Vec::new(),
            Vec::new(),
            true,
            false,
        );
        let settings = CommandResult::passed(
            "mom_llama.settings_get",
            "contracted",
            Settings::defaults_for_data_dir(std::env::temp_dir()),
            Vec::new(),
            Vec::new(),
            false,
            false,
        );
        let draft = CommandResult::passed(
            "mom_llama.draft_get",
            "contracted",
            DraftMessage {
                conversation_id: Some("conversation".to_string()),
                message: "Read these".to_string(),
                attachment_ids: vec!["image".to_string(), "notes".to_string()],
                updated_at: "1".to_string(),
            },
            Vec::new(),
            Vec::new(),
            false,
            false,
        );
        let attachments = vec![
            attachment_record("notes", "", "notes.md", AttachmentKind::Text),
            attachment_record("image", "", "garden.png", AttachmentKind::Image),
        ];
        let html = composer(&engine, &settings, Some(&draft), &attachments).into_string();
        assert!(html.contains(r#"id="composer-attachments" class="composer-attachments""#));
        assert!(html.contains(r#"data-staged-attachment-id="image""#));
        assert!(html.contains(r#"data-staged-attachment-id="notes""#));
        assert!(html.contains(r#"data-action="draft-attachment-remove""#));
        assert!(html.contains(r#"data-command="mom_llama.draft_update""#));
        assert!(html.find("garden.png") < html.find("notes.md"));
        assert!(html.contains("Read these"));
    }

    fn attachment_record(
        id: &str,
        message_id: &str,
        file_name: &str,
        kind: AttachmentKind,
    ) -> AttachmentRecord {
        AttachmentRecord {
            id: id.to_string(),
            conversation_id: "conversation".to_string(),
            message_id: message_id.to_string(),
            kind,
            file_name: file_name.to_string(),
            source_path: file_name.to_string(),
            stored_path: format!("encrypted://attachment.blob.{id}"),
            mime: "application/octet-stream".to_string(),
            bytes: 42,
            sha256: "0".repeat(64),
            created_at: "1".to_string(),
            state: mom_llama_runtime::AttachmentState::Staged,
            root_object_id: None,
            detected_format: None,
            coverage: None,
            manifest_namespace: None,
            policy_fingerprint: None,
            artifact_count: 1,
            canonical_text_bytes: 0,
            media_objects: 0,
        }
    }

    fn test_conversation(
        id: &str,
        title: &str,
        kind: ConversationKind,
        messages: Vec<Message>,
    ) -> Conversation {
        Conversation {
            id: id.to_string(),
            title: title.to_string(),
            created_at: "1".to_string(),
            updated_at: "1".to_string(),
            kind,
            execution_profile: Default::default(),
            selected_model_path: None,
            source_conversation_id: None,
            source_message_id: None,
            branch_root_message_id: None,
            active_leaf_message_id: messages.last().map(|message| message.id.clone()),
            current_skill_ids: Vec::new(),
            messages,
        }
    }

    fn test_message(id: &str, role: MessageRole, content: &str) -> Message {
        Message {
            id: id.to_string(),
            conversation_id: "conversation".to_string(),
            role,
            content: content.to_string(),
            created_at: "1".to_string(),
            parent_id: None,
            model: None,
            receipt_id: None,
            prompt_tokens: None,
            completion_tokens: None,
            reasoning_content: None,
            reasoning_incomplete: false,
            branch_index: None,
            branch_count: None,
            attribution: None,
            attachment_ids: Vec::new(),
        }
    }

    fn assert_dynamic_js_contract(js: &str, key: &str, control: &ControlSpec) {
        let marker = format!("{key}: Object.freeze({{");
        let start = js
            .find(&marker)
            .unwrap_or_else(|| panic!("missing dynamic contract {key}"));
        let end = js[start..]
            .find("\n    }),")
            .map(|offset| start + offset)
            .unwrap_or_else(|| panic!("unterminated dynamic contract {key}"));
        let block = &js[start..end];
        for expected in [
            format!(r#"affordance: "{}""#, control.affordance),
            format!(r#"command: "{}""#, control.command),
            format!(r#"tauri: "{}""#, control.tauri_command),
            format!(r#"cli: "{}""#, control.cli),
            format!(r#"effect: "{}""#, control.effect),
        ] {
            assert!(
                block.contains(&expected),
                "dynamic contract {key} diverged from CONTROL_SPECS: missing {expected}"
            );
        }
    }

    fn assert_command_precedes_local_mutation(
        js: &str,
        handler: &str,
        command: &str,
        mutation: &str,
    ) {
        let start = js
            .find(handler)
            .unwrap_or_else(|| panic!("missing dynamic action handler {handler}"));
        let end = js[start + handler.len()..]
            .find("\n    \"")
            .map(|offset| start + handler.len() + offset)
            .unwrap_or(js.len());
        let block = &js[start..end];
        let command = block
            .find(command)
            .unwrap_or_else(|| panic!("{handler} does not invoke {command}"));
        let mutation = block
            .find(mutation)
            .unwrap_or_else(|| panic!("{handler} does not perform {mutation}"));
        assert!(
            command < mutation,
            "{handler} mutates the view before validating its Rust command"
        );
    }

    fn assert_interactive_tags_have_metadata(html: &str, tag: &str) {
        let needle = format!("<{tag}");
        let mut rest = html;
        while let Some(index) = rest.find(&needle) {
            let after = &rest[index..];
            let end = after
                .find('>')
                .unwrap_or_else(|| panic!("unterminated <{tag}> tag"));
            let start_tag = &after[..=end];
            let command = attribute(start_tag, "data-command")
                .unwrap_or_else(|| panic!("<{tag}> missing command metadata: {start_tag}"));
            let _affordance = attribute(start_tag, "data-affordance")
                .unwrap_or_else(|| panic!("<{tag}> missing affordance metadata: {start_tag}"));
            let tauri_command = attribute(start_tag, "data-tauri-command")
                .unwrap_or_else(|| panic!("<{tag}> missing Tauri metadata: {start_tag}"));
            let cli = attribute(start_tag, "data-cli")
                .unwrap_or_else(|| panic!("<{tag}> missing CLI metadata: {start_tag}"));
            let effect = attribute(start_tag, "data-effect")
                .unwrap_or_else(|| panic!("<{tag}> missing effect metadata: {start_tag}"));
            assert!(
                CONTROL_SPECS.iter().any(|control| {
                    control.command == command
                        && control.tauri_command == tauri_command
                        && cli_verb(control.cli) == cli_verb(&decode_html_attribute(cli))
                        && control.effect == effect
                }),
                "<{tag}> advertises a command tuple absent from CONTROL_SPECS: {start_tag}"
            );
            rest = &after[end + 1..];
        }
    }

    fn attribute<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
        let prefix = format!(r#"{name}=""#);
        let value = tag.split_once(&prefix)?.1;
        value.split_once('"').map(|(value, _)| value)
    }

    fn decode_html_attribute(value: &str) -> String {
        value
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&#39;", "'")
            .replace("&amp;", "&")
    }

    fn cli_verb(value: &str) -> String {
        value
            .split_whitespace()
            .take_while(|token| !token.starts_with("--") && !token.starts_with('<'))
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn assert_buttons_use_approved_components(html: &str) {
        const APPROVED: &[&str] = &[
            "icon-button",
            "round-button",
            "send-button",
            "nav-button",
            "section-tab",
            "conversation-item",
            "small-button",
            "primary-button",
            "secondary-button",
            "message-action",
            "text-button",
            "chip-button",
            "mention-synthesize",
            "persona-picker-option",
            "consult-group-option",
            "persona-select",
            "model-row",
            "skill-row",
        ];
        let mut rest = html;
        while let Some(index) = rest.find("<button") {
            let after = &rest[index..];
            let end = after
                .find('>')
                .unwrap_or_else(|| panic!("unterminated <button> tag"));
            let start_tag = &after[..=end];
            assert!(
                APPROVED
                    .iter()
                    .any(|class| start_tag.contains(&format!(r#"class="{class}"#))
                        || start_tag.contains(&format!(" {class}"))),
                "button does not use an approved visual component: {start_tag}"
            );
            rest = &after[end + 1..];
        }
    }
}
