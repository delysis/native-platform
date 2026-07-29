use anyhow::Result;
use maud::{Markup, PreEscaped, html};
use mom_llama_runtime::{
    CommandResult, Conversation, DraftMessage, EngineCheckOptions, KvCachePolicy, Message,
    MessageRole, Settings, kv_cache::KvCacheStatus, models::ModelInfo, skill_store::Skill,
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
        affordance: "chat.composer.send",
        command: "mom_llama.chat_send",
        tauri_command: "mom_llama_chat_send",
        cli: "mom-llama chat send --conversation <id> --message <text> --json",
        effect: "mom_llama.effects.chat_send.v1",
        label: "Send",
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
        affordance: "consult.open",
        command: "mom_llama.consult_panel_list",
        tauri_command: "mom_llama_consult_panel_list",
        cli: "mom-llama consult panel-list --json",
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
        affordance: "consult.start",
        command: "mom_llama.consult_start",
        tauri_command: "mom_llama_consult_start",
        cli: "mom-llama consult start --conversation <id> --prompt <text> --stream-jsonl",
        effect: "mom_llama.effects.consult_generate.v1",
        label: "Ask consult group",
    },
    ControlSpec {
        affordance: "consult.cancel",
        command: "mom_llama.consult_cancel",
        tauri_command: "mom_llama_consult_cancel",
        cli: "mom-llama consult cancel --run <id> --seat <id> --json",
        effect: "mom_llama.effects.consult_cancel.v1",
        label: "Stop this perspective",
    },
    ControlSpec {
        affordance: "consult.synthesize",
        command: "mom_llama.consult_synthesize",
        tauri_command: "mom_llama_consult_synthesize",
        cli: "mom-llama consult synthesize --run <id> --json",
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
        label: "Save settings",
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
        affordance: "tool_loop.run",
        command: "mom_llama.tool_loop_run",
        tauri_command: "mom_llama_tool_loop_run",
        cli: "mom-llama tool-loop run --conversation <id> --prompt <text> --server <name> --tool <name> --arguments <json> --json",
        effect: "mom_llama.effects.tool_loop.v1",
        label: "Run tool loop",
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
        blocker: Some("External tools are available only through typed MCP command contracts."),
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
        label: "System Message",
        kind: "textarea",
        help: "Starting message that defines how the model should behave.",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
    SettingsFieldSpec {
        section: "general",
        key: "pasteLongTextToFileLen",
        label: "Paste long text to file length",
        kind: "number",
        help: "Length threshold for converting pasted text into an attachment.",
        options: EMPTY_OPTIONS,
        blocker: Some(
            "Attachments are limited to explicit local text import in this native shell.",
        ),
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
        help: "Preserve upstream copy behavior preference.",
        options: EMPTY_OPTIONS,
        blocker: Some("Rich attachment copy is not enabled in the native shell yet."),
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
        key: "askForTitleConfirmation",
        label: "Ask before changing title",
        kind: "checkbox",
        help: "Ask before automatically changing a conversation title.",
        options: EMPTY_OPTIONS,
        blocker: Some(
            "The native shell does not use blocking confirmation dialogs; automatic first-line titles remain opt-in.",
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
        key: "showThoughtInProgress",
        label: "Show thought in progress",
        kind: "checkbox",
        help: "Preserve upstream reasoning visibility preference.",
        options: EMPTY_OPTIONS,
        blocker: Some(
            "Reasoning-block parsing is not enabled for the current native model profile.",
        ),
    },
    SettingsFieldSpec {
        section: "display",
        key: "showToolCallInProgress",
        label: "Show tool call in progress",
        kind: "checkbox",
        help: "Show running tool calls.",
        options: EMPTY_OPTIONS,
        blocker: Some(
            "Tool execution is isolated in the MCP pane and is not injected into chat generation yet.",
        ),
    },
    SettingsFieldSpec {
        section: "display",
        key: "keepStatsVisible",
        label: "Keep stats visible",
        kind: "checkbox",
        help: "Keep stats visible after generation.",
        options: EMPTY_OPTIONS,
        blocker: Some("Native message statistics are either shown or hidden with Show statistics."),
    },
    SettingsFieldSpec {
        section: "display",
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
        key: "showSystemMessage",
        label: "Show system message",
        kind: "checkbox",
        help: "Display the system message at the top of each conversation.",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
    SettingsFieldSpec {
        section: "display",
        key: "alwaysShowAgenticTurns",
        label: "Always show agentic turns",
        kind: "checkbox",
        help: "Show hidden agentic turns.",
        options: EMPTY_OPTIONS,
        blocker: Some(
            "Agentic turns are not hidden in the current bounded MCP flow, so this setting has no effect.",
        ),
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
        help: "Maximum upstream agentic turns.",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
    SettingsFieldSpec {
        section: "agentic",
        key: "agenticMaxToolPreviewLines",
        label: "Max lines per tool preview",
        kind: "number",
        help: "Maximum tool preview lines.",
        options: EMPTY_OPTIONS,
        blocker: None,
    },
    SettingsFieldSpec {
        section: "mcp",
        key: "mcpNativeEnabled",
        label: "Enable local MCP adapters",
        kind: "checkbox",
        help: "Allow explicitly configured local MCP stdio executables to run with bounded requests.",
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
        key: "preEncodeConversation",
        label: "Pre-fill KV cache after response",
        kind: "checkbox",
        help: "Upstream KV pre-encode preference.",
        options: EMPTY_OPTIONS,
        blocker: Some("KV cache persistence is surfaced as honest status, not auto prefill yet."),
    },
    SettingsFieldSpec {
        section: "developer",
        key: "disableReasoningParsing",
        label: "Disable reasoning parsing",
        kind: "checkbox",
        help: "Do not parse reasoning content specially.",
        options: EMPTY_OPTIONS,
        blocker: Some(
            "Reasoning-block parsing is not enabled for the current native model profile.",
        ),
    },
    SettingsFieldSpec {
        section: "developer",
        key: "excludeReasoningFromContext",
        label: "Exclude reasoning from context",
        kind: "checkbox",
        help: "Exclude reasoning blocks from future context.",
        options: EMPTY_OPTIONS,
        blocker: Some(
            "Reasoning blocks are not parsed separately, so they cannot be excluded selectively.",
        ),
    },
    SettingsFieldSpec {
        section: "developer",
        key: "showRawOutputSwitch",
        label: "Enable raw output toggle",
        kind: "checkbox",
        help: "Expose raw output display mode.",
        options: EMPTY_OPTIONS,
        blocker: Some("The native chat view currently renders the authoritative model output."),
    },
    SettingsFieldSpec {
        section: "developer",
        key: "custom",
        label: "Custom JSON",
        kind: "textarea",
        help: "Custom upstream-compatible JSON settings.",
        options: EMPTY_OPTIONS,
        blocker: Some("Custom JSON is persisted but not allowed to inject arbitrary engine flags."),
    },
];

pub fn render_app() -> Result<String> {
    let settings = mom_llama_runtime::settings_get()?;
    let engine = mom_llama_runtime::engine_check(EngineCheckOptions::default())?;
    let conversations = mom_llama_runtime::conversation_list()?;
    let skills = mom_llama_runtime::skill_store::skill_list()?;
    let models = mom_llama_runtime::model_list()?;
    let kv = mom_llama_runtime::kv_cache_status()?;
    let selected_conversation_id = mom_llama_runtime::conversation_store::load_db()
        .ok()
        .and_then(|db| db.selected_conversation_id);
    Ok(app_markup(
        &settings,
        &engine,
        &conversations,
        &skills,
        &models,
        &kv,
        selected_conversation_id.as_deref(),
    )
    .into_string())
}

pub fn render_chat_fragment() -> Result<String> {
    let settings = mom_llama_runtime::settings_get()?;
    let engine = mom_llama_runtime::engine_check(EngineCheckOptions::default())?;
    let conversations = mom_llama_runtime::conversation_list()?;
    let selected = mom_llama_runtime::conversation_store::load_db()
        .ok()
        .and_then(|db| db.selected_conversation_id);
    let active = active_conversation(&conversations, selected.as_deref());
    Ok(chat_view(&settings, &engine, active).into_string())
}

pub fn render_sidebar_fragment() -> Result<String> {
    let conversations = mom_llama_runtime::conversation_list()?;
    let selected = mom_llama_runtime::conversation_store::load_db()
        .ok()
        .and_then(|db| db.selected_conversation_id);
    Ok(sidebar(&conversations, selected.as_deref()).into_string())
}

pub fn render_settings_fragment() -> Result<String> {
    let settings = mom_llama_runtime::settings_get()?;
    let engine = mom_llama_runtime::engine_check(EngineCheckOptions::default())?;
    let models = mom_llama_runtime::model_list()?;
    let skills = mom_llama_runtime::skill_store::skill_list()?;
    let kv = mom_llama_runtime::kv_cache_status()?;
    Ok(settings_modal(&settings, &engine, &models, &skills, &kv).into_string())
}

fn app_markup(
    settings: &CommandResult<Settings>,
    engine: &CommandResult<impl Serialize>,
    conversations: &CommandResult<Vec<Conversation>>,
    skills: &CommandResult<Vec<Skill>>,
    models: &CommandResult<Vec<ModelInfo>>,
    kv: &CommandResult<KvCacheStatus>,
    selected_conversation_id: Option<&str>,
) -> Markup {
    let active = active_conversation(conversations, selected_conversation_id);
    let theme = upstream_settings_value(settings, "theme");
    let full_height_code = upstream_settings_bool(settings, "fullHeightCodeBlocks");
    let disable_auto_scroll = upstream_settings_bool(settings, "disableAutoScroll");
    let always_show_sidebar = upstream_settings_bool(settings, "alwaysShowSidebarOnDesktop");
    let current_conversation_id = active
        .map(|conversation| conversation.id.as_str())
        .unwrap_or("default");
    let draft = mom_llama_runtime::draft_get(Some(current_conversation_id)).ok();
    html! {
        div class=(format!(
                "llama-ui-shell{}",
                if full_height_code { " full-height-code" } else { "" }
            ))
            data-theme=(theme)
            data-disable-auto-scroll=(disable_auto_scroll)
            data-always-show-sidebar=(always_show_sidebar)
            data-runtime="tauri-maud-htmx"
            data-native-core-only="true" {
            (contract_registry())
            (sidebar(conversations, active.map(|conversation| conversation.id.as_str())))
            header class="chrome" {
                (button("layout.sidebar_toggle", Some("sidebar-toggle"), "icon-button sidebar-toggle", false))
                (button("settings.open", Some("settings-open"), "icon-button settings-toggle", false))
            }
            (chat_view_with_draft(settings, engine, active, draft.as_ref()))
            (consult_view(engine, current_conversation_id, settings))
            (settings_modal(settings, engine, models, skills, kv))
            div id="command-status" class="command-status is-hidden" role="status" aria-live="polite" {}
            output id="command-output" class="sr-command-output" aria-live="polite" {}
        }
    }
}

fn chat_view(
    settings: &CommandResult<Settings>,
    engine: &CommandResult<impl Serialize>,
    active: Option<&Conversation>,
) -> Markup {
    let current_id = active
        .map(|conversation| conversation.id.as_str())
        .unwrap_or("default");
    let draft = mom_llama_runtime::draft_get(Some(current_id)).ok();
    chat_view_with_draft(settings, engine, active, draft.as_ref())
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
    html! {
        main id="chat" class=(format!("chat-main {}", if empty { "empty" } else { "has-messages" }))
            aria-label="Chat interface" data-current-conversation=(current_id) {
            @if empty {
                section class="landing" aria-label="Empty chat" {
                    h1 { "llama.cpp" }
                    p { "Type a message or upload files to get started" }
                }
            } @else {
                section class="message-stream" aria-label="Messages" {
                    @for message in active.map(|conversation| conversation.messages.as_slice()).unwrap_or(&[]) {
                        @if message.role != MessageRole::System || upstream_settings_bool(settings, "showSystemMessage") {
                            (message_row(message, settings))
                        }
                    }
                }
            }
            (composer(engine, settings, draft))
        }
    }
}

fn contract_registry() -> Markup {
    html! {
        template id="command-contract-registry" data-purpose="deterministic-ui-probe" {
            @for control in CONTROL_SPECS {
                span
                    data-affordance=(control.affordance)
                    data-command=(control.command)
                    data-tauri-command=(control.tauri_command)
                    data-cli=(control.cli)
                    data-effect=(control.effect) {}
            }
        }
    }
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
                (button("consult.open", Some("consult-open"), "nav-button", false))
                (button("skills.list", Some("skills-open"), "nav-button", false))
                (button("mcp.status", Some("mcp-status"), "nav-button", false))
            }
            section class="conversation-block" aria-label="Conversations" {
                h3 { "Conversations" }
                ol id="conversation-search-results" class="conversation-list search-results is-hidden" aria-live="polite" {}
                ol id="conversation-list" class="conversation-list" {
                    @if conversations.result.as_deref().unwrap_or(&[]).is_empty() {
                        li class="empty-line" { "No conversations yet" }
                    }
                    @for conversation in conversations.result.as_deref().unwrap_or(&[]) {
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

fn composer(
    engine: &CommandResult<impl Serialize>,
    settings: &CommandResult<Settings>,
    draft: Option<&CommandResult<DraftMessage>>,
) -> Markup {
    let enabled = engine.status == "host_integrated";
    let draft_message = draft
        .and_then(|draft| draft.result.as_ref())
        .map(|draft| draft.message.as_str())
        .unwrap_or_default();
    html! {
        form id="chat-form" class="composer"
            data-affordance="chat.composer.form"
            data-command="mom_llama.chat_send"
            data-tauri-command="mom_llama_chat_send"
            data-cli="mom-llama chat send"
            data-effect="mom_llama.effects.chat_send.v1" {
            textarea name="message"
                rows="2"
                aria-label="Message"
                placeholder="Type a message..."
                data-affordance="chat.composer.message"
                data-command="mom_llama.chat_send"
                data-tauri-command="mom_llama_chat_send"
                data-cli="mom-llama chat send"
                data-effect="mom_llama.effects.chat_send.v1"
                data-draft-affordance="conversation.draft_update"
                data-draft-command="mom_llama.draft_update"
                data-draft-tauri-command="mom_llama_draft_update"
                data-draft-cli="mom-llama conversation draft-update --conversation <id> --message <text> --json"
                data-draft-effect="mom_llama.effects.conversation_store.v1" {
                (draft_message)
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
                    (button("chat.composer.cancel", Some("chat-cancel"), "send-button stop-button is-hidden", true))
                    button type="submit"
                        class="send-button"
                        data-affordance="chat.composer.send"
                        data-command="mom_llama.chat_send"
                        data-tauri-command="mom_llama_chat_send"
                        data-cli="mom-llama chat send --conversation <id> --message <text> --json"
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

fn consult_view(
    engine: &CommandResult<impl Serialize>,
    conversation_id: &str,
    settings: &CommandResult<Settings>,
) -> Markup {
    let enabled = engine.status == "host_integrated";
    let seats = [
        (
            "evidence",
            "Evidence lens",
            "Separates observations, evidence, and uncertainty.",
        ),
        (
            "whole-person",
            "Whole-person lens",
            "Considers goals, context, preferences, and tradeoffs.",
        ),
        (
            "practical",
            "Practical lens",
            "Turns reasoning into safe, concrete next steps.",
        ),
        (
            "skeptical",
            "Skeptical lens",
            "Challenges assumptions and searches for failure modes.",
        ),
    ];
    let cancel = control("consult.cancel");
    let synthesize = control("consult.synthesize");
    html! {
        section id="consult-view" class="consult-view is-hidden" aria-label="Consult group"
            data-current-conversation=(conversation_id) data-run-id="" {
            header class="consult-header" {
                div {
                    p class="consult-eyebrow" { "Private local reasoning panel" }
                    h1 { "Consult group" }
                    p { "Four perspectives reason in parallel. They are not clinicians or medical authorities." }
                }
                div class="consult-header-actions" {
                    span class="model-pill" { (icon_markup("box")) (model_chip_label(settings)) }
                    (button("consult.close", Some("consult-close"), "icon-button", false))
                }
            }
            div id="consult-grid" class="consult-grid" {
                @for (seat_id, label, description) in seats {
                    article class="consult-seat" data-seat=(seat_id) data-state="idle" {
                        header {
                            div {
                                h2 { (label) }
                                p { (description) }
                            }
                            span class="seat-state" { "Ready" }
                        }
                        div class="seat-output" aria-live="polite" {
                            p class="seat-placeholder" { "This perspective will appear here." }
                        }
                        footer {
                            span class="seat-model" { (model_chip_label(settings)) }
                            button type="button" class="icon-button seat-stop"
                                title=(cancel.label)
                                data-affordance=(cancel.affordance)
                                data-command=(cancel.command)
                                data-tauri-command=(cancel.tauri_command)
                                data-cli=(cancel.cli)
                                data-effect=(cancel.effect)
                                data-action="consult-cancel"
                                data-seat=(seat_id)
                                disabled {
                                (icon_markup("square")) span class="sr-only" { (cancel.label) }
                            }
                        }
                    }
                }
            }
            section id="consult-synthesis" class="consult-synthesis is-hidden" aria-live="polite" {
                header { h2 { "Synthesis" } span { "Derived from selected perspectives" } }
                div class="synthesis-output" {}
            }
            form id="consult-form" class="consult-composer"
                data-affordance="consult.start"
                data-command="mom_llama.consult_start"
                data-tauri-command="mom_llama_consult_start"
                data-cli="mom-llama consult start --conversation <id> --prompt <text> --stream-jsonl"
                data-effect="mom_llama.effects.consult_generate.v1" {
                textarea name="prompt" rows="2" placeholder="What should the consult group consider?"
                    aria-label="Consult group question"
                    data-affordance="consult.start"
                    data-command="mom_llama.consult_start"
                    data-tauri-command="mom_llama_consult_start"
                    data-cli="mom-llama consult start --conversation <id> --prompt <text> --stream-jsonl"
                    data-effect="mom_llama.effects.consult_generate.v1";
                div class="consult-composer-actions" {
                    button id="consult-synthesize-button" type="button" class="secondary-button"
                        data-affordance=(synthesize.affordance)
                        data-command=(synthesize.command)
                        data-tauri-command=(synthesize.tauri_command)
                        data-cli=(synthesize.cli)
                        data-effect=(synthesize.effect)
                        data-action="consult-synthesize" disabled {
                        (icon_markup("sparkles")) span { (synthesize.label) }
                    }
                    button type="submit" class="primary-button"
                        data-affordance="consult.start"
                        data-command="mom_llama.consult_start"
                        data-tauri-command="mom_llama_consult_start"
                        data-cli="mom-llama consult start --conversation <id> --prompt <text> --stream-jsonl"
                        data-effect="mom_llama.effects.consult_generate.v1"
                        disabled[!enabled] {
                        span { "Ask four perspectives" } (icon_markup("arrow-up"))
                    }
                }
            }
        }
    }
}

fn message_row(message: &Message, settings: &CommandResult<Settings>) -> Markup {
    let role = role_label(&message.role);
    let show_stats = upstream_settings_bool(settings, "showMessageStats");
    let enable_continue = upstream_settings_bool(settings, "enableContinueGeneration");
    let user_markdown = upstream_settings_bool(settings, "renderUserContentAsMarkdown");
    let show_raw_model = upstream_settings_bool(settings, "showRawModelNames");
    let model = if show_raw_model {
        message
            .model
            .clone()
            .unwrap_or_else(|| model_label(settings))
    } else {
        model_chip_label(settings)
    };
    html! {
        article class=(format!("message-row {role}")) aria-label=(format!("{role} message")) {
            div class="message-card" {
                @if message.role == MessageRole::User && !user_markdown {
                    p class="plain-message-content" { (message.content) }
                } @else {
                    (markdown_content(&message.content))
                }
                @if message.role == MessageRole::Assistant && show_stats {
                    p class="message-model" { (model) }
                }
            }
            (message_actions(message, message.role == MessageRole::Assistant, enable_continue))
        }
    }
}

fn message_actions(message: &Message, assistant: bool, enable_continue: bool) -> Markup {
    html! {
        div class="message-actions" {
            (message_button("message.copy", "message-copy", message))
            (message_button("message.edit", "message-edit", message))
            @if assistant {
                (message_button("chat.message.regenerate", "chat-regenerate", message))
                @if enable_continue {
                    (message_button("chat.message.continue", "chat-continue", message))
                }
            }
            (message_button("conversation.fork", "conversation-fork", message))
            (message_button("conversation.siblings", "conversation-siblings", message))
            (message_button("message.delete", "message-delete", message))
        }
    }
}

fn message_button(key: &str, action: &str, message: &Message) -> Markup {
    let control = control(key);
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
            (control.label)
        }
    }
}

fn settings_modal(
    settings: &CommandResult<Settings>,
    engine: &CommandResult<impl Serialize>,
    models: &CommandResult<Vec<ModelInfo>>,
    skills: &CommandResult<Vec<Skill>>,
    kv: &CommandResult<KvCacheStatus>,
) -> Markup {
    html! {
        div id="settings-modal" class="modal-backdrop is-hidden" hidden[true] aria-hidden="true" {
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
                            (settings_panel(section, settings, engine, models, skills, kv))
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
                                    option value="none" { "No cache" }
                                    option value="prompt_prefix" { "Prompt prefix" }
                                    option value="kv_cache_candidate" { "KV cache candidate" }
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
                    (button("settings.update", Some("settings-update"), "save-button", false))
                }
            }
        }
    }
}

fn settings_panel(
    section: &SettingsSectionSpec,
    settings: &CommandResult<Settings>,
    _engine: &CommandResult<impl Serialize>,
    _models: &CommandResult<Vec<ModelInfo>>,
    _skills: &CommandResult<Vec<Skill>>,
    _kv: &CommandResult<KvCacheStatus>,
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
                section class="settings-card native-runtime" {
                    h3 { "Native runtime" }
                    (settings_path_input("Model path", "model_path", settings_value(settings, "model_path"), "model-browse"))
                    (settings_path_input("Vision projector", "mmproj_path", settings_value(settings, "mmproj_path"), "mmproj-browse"))
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
                    h3 { "Tools" }
                    p class="field-help" {
                        "Tools run through configured MCP stdio servers with bounded requests."
                    }
                    div class="button-strip" {
                        (button("mcp.list_tools", Some("mcp-list-tools"), "small-button", false))
                        (button("mcp.call_tool", Some("mcp-call-tool"), "small-button", false))
                        (button("tool_loop.run", Some("tool-loop-run"), "small-button", false))
                    }
                }
            }
            div class="settings-field-list" {
                @for field in SETTINGS_FIELDS.iter().filter(|field| field.section == section.slug) {
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
                                data-affordance="tool_loop.run" data-command="mom_llama.tool_loop_run"
                                data-tauri-command="mom_llama_tool_loop_run"
                                data-cli="mom-llama tool-loop run --prompt <text> --json"
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
                    }
                }
            @if section.slug == "developer" {
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
            input name=(name) value=(value)
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
                    data-affordance="settings.update"
                    data-command="mom_llama.settings_update"
                    data-tauri-command="mom_llama_settings_update"
                    data-cli="mom-llama settings update --json"
                    data-effect="mom_llama.effects.settings_store.v1" {
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
                    data-affordance=(command.affordance)
                    data-command=(command.command)
                    data-tauri-command=(command.tauri_command)
                    data-cli=(command.cli)
                    data-effect=(command.effect) { (icon_markup("folder-open")) }
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
        "attachment.import_text" | "attachment.import" => Some("plus"),
        "attachment.list" => Some("database"),
        "chat.composer.send" => Some("arrow-up"),
        "chat.composer.cancel" => Some("square"),
        "consult.open" => Some("users"),
        "consult.close" => Some("x"),
        "consult.start" => Some("arrow-up"),
        "consult.cancel" => Some("square"),
        "consult.synthesize" => Some("sparkles"),
        "settings.reset" => Some("rotate-ccw"),
        "settings.update" => Some("save"),
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
        "trash-2" => r#"<path d="M3 6h18"/><path d="M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/><path d="M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6"/><path d="M10 11v6"/><path d="M14 11v6"/>"#,
        "sliders" => r#"<path d="M4 21v-7"/><path d="M4 10V3"/><path d="M12 21v-9"/><path d="M12 8V3"/><path d="M20 21v-5"/><path d="M20 12V3"/><path d="M2 14h4"/><path d="M10 8h4"/><path d="M18 16h4"/>"#,
        "monitor" => r#"<rect width="20" height="14" x="2" y="3" rx="2"/><path d="M8 21h8"/><path d="M12 17v4"/>"#,
        "funnel" => r#"<path d="M10 20a1 1 0 0 0 .55.9l2 1A1 1 0 0 0 14 21v-7a2 2 0 0 1 .6-1.4L21 6.2A2 2 0 0 0 19.6 3H4.4A2 2 0 0 0 3 6.2l6.4 6.4A2 2 0 0 1 10 14z"/>"#,
        "alert-triangle" => r#"<path d="m21.73 18-8-14a2 2 0 0 0-3.46 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3"/><path d="M12 9v4"/><path d="M12 17h.01"/>"#,
        "list-restart" => r#"<path d="M21 6H3"/><path d="M7 12H3"/><path d="M7 18H3"/><path d="M12 18a5 5 0 1 0-5-5"/><path d="M7 8v5h5"/>"#,
        "pencil-ruler" => r#"<path d="M13 7 8.7 2.7a2.4 2.4 0 0 0-3.4 0L2.7 5.3a2.4 2.4 0 0 0 0 3.4L7 13"/><path d="m8 6 2-2"/><path d="m18 16 2-2"/><path d="m17 11 4 4-6 6-4-4Z"/><path d="M14 14 4 4"/>"#,
        "pencil" => r#"<path d="M21.17 6.812a3 3 0 0 0-4.24-4.24L3 16.5V21h4.5Z"/><path d="m15 5 4 4"/>"#,
        "folder-open" => r#"<path d="m6 14 1.5-2.9A2 2 0 0 1 9.2 10H20a2 2 0 0 1 1.8 2.9l-2 4A2 2 0 0 1 18 18H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h3.9a2 2 0 0 1 1.7.9l.8 1.2A2 2 0 0 0 13.1 6H19a2 2 0 0 1 2 2v2"/>"#,
        "code" => r#"<path d="m16 18 6-6-6-6"/><path d="m8 6-6 6 6 6"/>"#,
        "square" => r#"<rect width="14" height="14" x="5" y="5" rx="2"/>"#,
        "users" => r#"<path d="M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2"/><circle cx="9" cy="7" r="4"/><path d="M22 21v-2a4 4 0 0 0-3-3.87"/><path d="M16 3.13a4 4 0 0 1 0 7.75"/>"#,
        "sparkles" => r#"<path d="m12 3-1.9 5.1L5 10l5.1 1.9L12 17l1.9-5.1L19 10l-5.1-1.9Z"/><path d="M5 3v4"/><path d="M3 5h4"/><path d="M19 17v4"/><path d="M17 19h4"/>"#,
        "check" => r#"<path d="M20 6 9 17l-5-5"/>"#,
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

fn active_conversation<'a>(
    conversations: &'a CommandResult<Vec<Conversation>>,
    selected_id: Option<&str>,
) -> Option<&'a Conversation> {
    let items = conversations.result.as_ref()?;
    selected_id
        .and_then(|id| items.iter().find(|conversation| conversation.id == id))
        .or_else(|| items.first())
}

fn role_label(role: &MessageRole) -> &'static str {
    match role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
    }
}

fn readiness_short_label(engine: &CommandResult<impl Serialize>) -> String {
    if engine.status == "host_integrated" {
        "Connected".to_string()
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
    label
        .trim_end_matches(".gguf")
        .split(['-', '_'])
        .take(2)
        .collect::<Vec<_>>()
        .join("-")
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

    #[test]
    fn rendered_app_controls_have_contract_metadata() -> Result<()> {
        let data_dir = std::env::temp_dir().join(format!(
            "mom-llama-view-test-{}",
            mom_llama_runtime::now_ms()
        ));
        mom_llama_runtime::config::set_data_dir_override_for_tests(Some(data_dir));
        let html = render_app()?;
        mom_llama_runtime::config::set_data_dir_override_for_tests(None);
        for control in CONTROL_SPECS {
            assert!(
                html.contains(&format!(r#"data-affordance="{}""#, control.affordance)),
                "missing affordance {}",
                control.affordance
            );
            assert!(
                html.contains(&format!(r#"data-command="{}""#, control.command)),
                "missing command {}",
                control.command
            );
            assert!(
                html.contains(&format!(
                    r#"data-tauri-command="{}""#,
                    control.tauri_command
                )),
                "missing tauri command {}",
                control.tauri_command
            );
            assert!(
                html.contains(&format!(r#"data-effect="{}""#, control.effect)),
                "missing effect {}",
                control.effect
            );
        }
        for forbidden in ["__sveltekit__", "React", "Vue", "fetch("] {
            assert!(
                !html.contains(forbidden),
                "found forbidden marker {forbidden}"
            );
        }
        for tag in ["button", "input", "textarea", "select", "form"] {
            assert_interactive_tags_have_metadata(&html, tag);
        }
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
        assert!(html.contains("lucide-search"));
        assert!(html.contains("mcp-logo"));
        let js = include_str!("../../ui/coop-hx.js");
        assert!(js.contains("renderSearchResults"));
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
        Ok(())
    }

    #[test]
    fn markdown_tables_render_without_frontend_runtime() {
        let html =
            markdown_content("| Name | Value |\n| --- | --- |\n| Cache | Ready |").into_string();
        assert!(html.contains(r#"<table class="markdown-table">"#));
        assert!(html.contains("<th>Name</th>"));
        assert!(html.contains("<td>Ready</td>"));
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
            assert!(
                start_tag.contains("data-command=")
                    && start_tag.contains("data-affordance=")
                    && start_tag.contains("data-effect="),
                "<{tag}> missing command metadata: {start_tag}"
            );
            rest = &after[end + 1..];
        }
    }
}
