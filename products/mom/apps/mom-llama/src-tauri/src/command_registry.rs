#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandClass {
    ReadOnly,
    Mutation,
    LongOperation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandSpec {
    pub name: &'static str,
    pub class: CommandClass,
    pub mutates_store: bool,
    pub starts_operation: bool,
    pub uses_gateway: bool,
    pub uses_native: bool,
    pub allowed_during_quiesce: bool,
    pub permission: &'static str,
}

const fn read(name: &'static str, writes_receipt: bool, uses_native: bool) -> CommandSpec {
    CommandSpec {
        name,
        class: CommandClass::ReadOnly,
        mutates_store: writes_receipt,
        starts_operation: false,
        uses_gateway: false,
        uses_native,
        allowed_during_quiesce: false,
        permission: "default",
    }
}

const fn mutation(name: &'static str, mutates_store: bool, uses_native: bool) -> CommandSpec {
    CommandSpec {
        name,
        class: CommandClass::Mutation,
        mutates_store,
        starts_operation: false,
        uses_gateway: false,
        uses_native,
        allowed_during_quiesce: false,
        permission: "default",
    }
}

const fn long(name: &'static str, mutates_store: bool, uses_native: bool) -> CommandSpec {
    CommandSpec {
        name,
        class: CommandClass::LongOperation,
        mutates_store,
        starts_operation: true,
        uses_gateway: false,
        uses_native,
        allowed_during_quiesce: false,
        permission: "default",
    }
}

pub static COMMAND_SPECS: &[CommandSpec] = &[
    read("mom_llama_render_app", false, true),
    read("mom_llama_render_chat_fragment", false, true),
    read("mom_llama_render_sidebar_fragment", false, false),
    read("mom_llama_render_persona_picker_fragment", false, false),
    read("mom_llama_render_settings_fragment", false, true),
    long("mom_llama_pick_file", false, false),
    long("mom_llama_engine_check", false, true),
    mutation("mom_llama_engine_configure", true, true),
    read("mom_llama_model_list", true, false),
    mutation("mom_llama_model_select", true, true),
    long("mom_llama_chat_send", true, true),
    long("mom_llama_chat_dispatch", true, true),
    long("mom_llama_mention_dispatch", true, true),
    read("mom_llama_mention_candidates", true, false),
    mutation("mom_llama_mention_cancel", false, true),
    long("mom_llama_mention_synthesize", true, true),
    mutation("mom_llama_persona_freeze", true, false),
    read("mom_llama_persona_list", true, false),
    read("mom_llama_persona_get", true, false),
    long("mom_llama_persona_update", true, true),
    mutation("mom_llama_persona_delete", true, false),
    mutation("mom_llama_persona_instantiate", true, false),
    read("mom_llama_persona_group_list", true, false),
    mutation("mom_llama_persona_group_create", true, false),
    mutation("mom_llama_persona_group_update", true, false),
    mutation("mom_llama_persona_group_delete", true, false),
    mutation("mom_llama_chat_cancel", true, true),
    mutation("mom_llama_chat_skip_reasoning", false, true),
    long("mom_llama_chat_regenerate", true, true),
    long("mom_llama_chat_continue", true, true),
    mutation("mom_llama_conversation_new", true, false),
    read("mom_llama_conversation_list", true, false),
    mutation("mom_llama_conversation_select", true, false),
    read("mom_llama_conversation_search", true, false),
    mutation("mom_llama_conversation_rename", true, false),
    mutation("mom_llama_conversation_system_message_update", true, false),
    mutation("mom_llama_conversation_delete", true, false),
    mutation("mom_llama_conversation_fork", true, false),
    read("mom_llama_conversation_siblings", true, false),
    read("mom_llama_draft_get", true, false),
    mutation("mom_llama_draft_update", true, false),
    mutation("mom_llama_draft_clear", true, false),
    read("mom_llama_conversation_export", true, false),
    mutation("mom_llama_conversation_import", true, false),
    read("mom_llama_message_copy", true, false),
    mutation("mom_llama_message_edit", true, false),
    mutation("mom_llama_message_delete", true, false),
    read("mom_llama_message_branches", true, false),
    mutation("mom_llama_message_branch_select", true, false),
    long("mom_llama_attachment_import_text", true, false),
    long("mom_llama_attachment_import_paste", true, false),
    long("mom_llama_attachment_import", true, false),
    read("mom_llama_attachment_list", true, false),
    long("mom_llama_attachment_preview", false, false),
    long("mom_llama_attachment_preview_bytes", false, false),
    read("mom_llama_settings_get", true, false),
    mutation("mom_llama_settings_reset", true, true),
    mutation("mom_llama_settings_update", true, true),
    mutation("mom_llama_skill_create", true, false),
    mutation("mom_llama_skill_update", true, false),
    read("mom_llama_skill_list", true, false),
    mutation("mom_llama_skill_apply", true, false),
    read("mom_llama_kv_cache_status", true, false),
    long("mom_llama_kv_cache_save", true, true),
    long("mom_llama_kv_cache_restore", true, true),
    long("mom_llama_kv_cache_clear", true, true),
    read("mom_llama_mcp_status", true, false),
    mutation("mom_llama_mcp_configure", true, false),
    read("mom_llama_mcp_list_servers", true, false),
    long("mom_llama_mcp_list_tools", false, false),
    long("mom_llama_mcp_call_tool", true, false),
    long("mom_llama_mcp_list_resources", false, false),
    long("mom_llama_mcp_read_resource", false, false),
    long("mom_llama_mcp_list_prompts", false, false),
    long("mom_llama_mcp_get_prompt", false, false),
    mutation("mom_llama_tool_loop_prepare", true, false),
    long("mom_llama_tool_loop_run", true, true),
    mutation("mom_llama_tool_loop_cancel", true, true),
    read("mom_llama_tool_loop_status", true, false),
    read("mom_llama_tool_permission_list", true, false),
    mutation("mom_llama_tool_permission_set", true, false),
    mutation("mom_llama_tool_permission_revoke", true, false),
    read("mom_llama_model_slot_list", true, true),
    long("mom_llama_model_slot_load", true, true),
    long("mom_llama_model_slot_unload", true, true),
];

pub fn command_spec(name: &str) -> &'static CommandSpec {
    COMMAND_SPECS
        .iter()
        .find(|spec| spec.name == name)
        .unwrap_or_else(|| panic!("Tauri command `{name}` is not classified"))
}

#[cfg(test)]
mod tests {
    use super::{COMMAND_SPECS, CommandClass};
    use std::collections::BTreeSet;

    fn names_between(source: &str, start: &str, end: &str, prefix: &str) -> BTreeSet<String> {
        let body = source
            .split_once(start)
            .unwrap_or_else(|| panic!("missing registry start `{start}`"))
            .1
            .split_once(end)
            .unwrap_or_else(|| panic!("missing registry end `{end}`"))
            .0;
        body.lines()
            .filter_map(|line| line.trim().strip_prefix(prefix))
            .filter_map(|line| line.split([',', ')']).next())
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn command_registry_matches_invoke_handler() {
        let main = include_str!("main.rs");
        let invoked = names_between(
            main,
            ".invoke_handler(tauri::generate_handler![",
            "])",
            "commands::",
        );
        let classified = COMMAND_SPECS
            .iter()
            .map(|spec| spec.name.to_string())
            .collect::<BTreeSet<_>>();
        assert_eq!(classified, invoked);
        assert_eq!(
            classified.len(),
            COMMAND_SPECS.len(),
            "duplicate command spec"
        );
    }

    #[test]
    fn contract_and_ui_registries_reference_classified_commands() {
        let classified = COMMAND_SPECS
            .iter()
            .map(|spec| spec.name)
            .collect::<BTreeSet<_>>();
        let contracts: serde_json::Value =
            serde_json::from_str(include_str!("../../../../contracts/commands.json"))
                .expect("command contracts JSON");
        for command in contracts["commands"]
            .as_array()
            .expect("command contracts array")
        {
            let tauri_command = command["tauri_command"]
                .as_str()
                .expect("contract Tauri command");
            assert!(
                classified.contains(tauri_command),
                "contract command {tauri_command} must be classified"
            );
        }

        let view = include_str!("view.rs");
        for line in view.lines() {
            let Some(value) = line.trim().strip_prefix("tauri_command: \"") else {
                continue;
            };
            let tauri_command = value.split_once('"').expect("quoted UI Tauri command").0;
            assert!(
                classified.contains(tauri_command),
                "UI command {tauri_command} must be classified"
            );
        }
    }

    #[test]
    fn command_semantics_match_product_authority() {
        let native_commands = BTreeSet::from([
            "mom_llama_render_app",
            "mom_llama_render_chat_fragment",
            "mom_llama_render_settings_fragment",
            "mom_llama_engine_check",
            "mom_llama_engine_configure",
            "mom_llama_model_select",
            "mom_llama_chat_send",
            "mom_llama_chat_dispatch",
            "mom_llama_mention_dispatch",
            "mom_llama_mention_cancel",
            "mom_llama_mention_synthesize",
            "mom_llama_persona_update",
            "mom_llama_chat_cancel",
            "mom_llama_chat_skip_reasoning",
            "mom_llama_chat_regenerate",
            "mom_llama_chat_continue",
            "mom_llama_settings_reset",
            "mom_llama_settings_update",
            "mom_llama_kv_cache_save",
            "mom_llama_kv_cache_restore",
            "mom_llama_kv_cache_clear",
            "mom_llama_tool_loop_run",
            "mom_llama_tool_loop_cancel",
            "mom_llama_model_slot_list",
            "mom_llama_model_slot_load",
            "mom_llama_model_slot_unload",
        ]);
        let non_store_mutations =
            BTreeSet::from(["mom_llama_mention_cancel", "mom_llama_chat_skip_reasoning"]);
        let non_store_long_operations = BTreeSet::from([
            "mom_llama_pick_file",
            "mom_llama_engine_check",
            "mom_llama_attachment_preview",
            "mom_llama_attachment_preview_bytes",
            "mom_llama_mcp_list_tools",
            "mom_llama_mcp_list_resources",
            "mom_llama_mcp_read_resource",
            "mom_llama_mcp_list_prompts",
            "mom_llama_mcp_get_prompt",
        ]);
        let command_source = include_str!("commands.rs");

        for spec in COMMAND_SPECS {
            assert_eq!(spec.permission, "default", "{} permission", spec.name);
            assert_eq!(
                spec.uses_native,
                native_commands.contains(spec.name),
                "{} native authority",
                spec.name
            );
            assert!(!spec.uses_gateway, "{} has no gateway call path", spec.name);
            assert_eq!(
                spec.starts_operation,
                spec.class == CommandClass::LongOperation,
                "{} operation lifetime",
                spec.name
            );
            assert!(!spec.allowed_during_quiesce, "{} quiesce policy", spec.name);
            let expected_store_mutation = match spec.class {
                CommandClass::ReadOnly => {
                    command_body(command_source, spec.name).contains("command_value(")
                }
                CommandClass::Mutation => !non_store_mutations.contains(spec.name),
                CommandClass::LongOperation => !non_store_long_operations.contains(spec.name),
            };
            assert_eq!(
                spec.mutates_store, expected_store_mutation,
                "{} store authority",
                spec.name
            );
        }
    }

    #[test]
    fn command_permission_matches_tauri_capability_identifier() {
        let capability: serde_json::Value =
            serde_json::from_str(include_str!("../capabilities/default.json"))
                .expect("Tauri capability JSON");
        let identifier = capability["identifier"]
            .as_str()
            .expect("capability identifier");
        assert!(
            COMMAND_SPECS
                .iter()
                .all(|spec| spec.permission == identifier)
        );
    }

    #[test]
    fn every_command_has_app_lease() {
        let source = include_str!("commands.rs");
        for spec in COMMAND_SPECS {
            assert!(
                command_body(source, spec.name)
                    .contains(&format!("admit(command_spec(\"{}\"))", spec.name)),
                "{} must atomically acquire its classified app lease",
                spec.name
            );
        }
    }

    #[test]
    fn every_long_operation_has_cancellation_and_app_lease() {
        let source = include_str!("commands.rs");
        for spec in COMMAND_SPECS
            .iter()
            .filter(|spec| spec.class == CommandClass::LongOperation)
        {
            let body = command_body(source, spec.name);
            assert!(
                body.contains(&format!("admit(command_spec(\"{}\"))", spec.name)),
                "{} must atomically acquire its classified app lease",
                spec.name
            );
            assert!(
                body.contains("blocking_command(")
                    || body.contains("blocking_response(")
                    || body.contains("lease.cancellation")
                    || body.contains("lease.cancelled()"),
                "{} must carry an application cancellation control",
                spec.name
            );
        }
    }

    fn command_body<'a>(source: &'a str, name: &str) -> &'a str {
        let sync = format!("pub fn {name}(");
        let asynchronous = format!("pub async fn {name}(");
        let start = source
            .find(&sync)
            .or_else(|| source.find(&asynchronous))
            .unwrap_or_else(|| panic!("missing Tauri command {name}"));
        let rest = &source[start..];
        let end = rest.find("\n#[tauri::command]").unwrap_or(rest.len());
        &rest[..end]
    }
}
