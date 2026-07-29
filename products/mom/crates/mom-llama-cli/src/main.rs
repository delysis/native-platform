use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use llama_native_types::NativeDevice;
use mom_llama_runtime::{
    ChatSendInput, ChatSendOptions, ConsultStartInput, ConsultStartOptions,
    ConversationExportFormat, EngineCheckOptions, KvCachePolicy, config::SettingsUpdate,
};
use serde::Serialize;
use serde_json::Value;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "mom-llama")]
#[command(about = "Mom Llama Lab local llama.cpp CLI")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Engine {
        #[command(subcommand)]
        command: EngineCommand,
    },
    Model {
        #[command(subcommand)]
        command: ModelCommand,
    },
    Chat {
        #[command(subcommand)]
        command: ChatCommand,
    },
    Consult {
        #[command(subcommand)]
        command: ConsultCommand,
    },
    Message {
        #[command(subcommand)]
        command: MessageCommand,
    },
    Attachment {
        #[command(subcommand)]
        command: AttachmentCommand,
    },
    #[command(hide = true)]
    Server {
        #[command(subcommand)]
        command: ServerCommand,
    },
    Conversation {
        #[command(subcommand)]
        command: ConversationCommand,
    },
    Settings {
        #[command(subcommand)]
        command: SettingsCommand,
    },
    Mcp {
        #[command(subcommand)]
        command: McpCommand,
    },
    ToolLoop {
        #[command(subcommand)]
        command: ToolLoopCommand,
    },
    Skill {
        #[command(subcommand)]
        command: SkillCommand,
    },
    KvCache {
        #[command(subcommand)]
        command: KvCacheCommand,
    },
}

#[derive(Debug, Subcommand)]
enum EngineCommand {
    Check {
        #[arg(long)]
        json: bool,
    },
    Configure {
        #[arg(long)]
        model_path: PathBuf,
        #[arg(long, value_enum)]
        device: Option<NativeDeviceArg>,
        #[arg(long)]
        context_tokens: Option<u32>,
        #[arg(long)]
        batch_tokens: Option<u32>,
        #[arg(long)]
        max_parallel_sequences: Option<u32>,
        #[arg(long)]
        memory_budget_mib: Option<u64>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum ModelCommand {
    List {
        #[arg(long)]
        json: bool,
    },
    Select {
        #[arg(long)]
        model_path: PathBuf,
        #[arg(long)]
        json: bool,
    },
    Status {
        #[arg(long)]
        json: bool,
    },
    Load {
        #[arg(long, default_value_t = 0)]
        slot: usize,
        #[arg(long)]
        model_path: PathBuf,
        #[arg(long)]
        json: bool,
    },
    Unload {
        #[arg(long, default_value_t = 0)]
        slot: usize,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum ChatCommand {
    Send {
        #[arg(long)]
        conversation: String,
        #[arg(long)]
        message: String,
        #[arg(long = "timeout-s")]
        timeout_s: Option<f64>,
        #[arg(long = "stream-jsonl")]
        stream_jsonl: bool,
        #[arg(long)]
        json: bool,
    },
    Cancel {
        #[arg(long)]
        conversation: String,
        #[arg(long)]
        json: bool,
    },
    Regenerate {
        #[arg(long)]
        conversation: String,
        #[arg(long = "timeout-s")]
        timeout_s: Option<f64>,
        #[arg(long)]
        json: bool,
    },
    Continue {
        #[arg(long)]
        conversation: String,
        #[arg(long = "timeout-s")]
        timeout_s: Option<f64>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum ConsultCommand {
    PanelList {
        #[arg(long)]
        json: bool,
    },
    Start {
        #[arg(long)]
        conversation: String,
        #[arg(long)]
        prompt: String,
        #[arg(long)]
        panel: Option<String>,
        #[arg(long = "timeout-s")]
        timeout_s: Option<f64>,
        #[arg(long = "stream-jsonl")]
        stream_jsonl: bool,
        #[arg(long)]
        json: bool,
    },
    Status {
        #[arg(long)]
        run: String,
        #[arg(long)]
        json: bool,
    },
    Cancel {
        #[arg(long)]
        run: String,
        #[arg(long)]
        seat: Option<String>,
        #[arg(long)]
        json: bool,
    },
    Synthesize {
        #[arg(long)]
        run: String,
        #[arg(long = "seat")]
        seats: Vec<String>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum ConversationCommand {
    New {
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        json: bool,
    },
    List {
        #[arg(long)]
        json: bool,
    },
    Select {
        #[arg(long)]
        conversation: String,
        #[arg(long)]
        json: bool,
    },
    Search {
        #[arg(long, default_value = "")]
        query: String,
        #[arg(long)]
        json: bool,
    },
    Rename {
        #[arg(long)]
        conversation: String,
        #[arg(long)]
        title: String,
        #[arg(long)]
        json: bool,
    },
    Delete {
        #[arg(long)]
        conversation: String,
        #[arg(long)]
        json: bool,
    },
    Fork {
        #[arg(long)]
        conversation: String,
        #[arg(long)]
        message: String,
        #[arg(long)]
        json: bool,
    },
    Siblings {
        #[arg(long)]
        conversation: String,
        #[arg(long)]
        json: bool,
    },
    DraftGet {
        #[arg(long)]
        conversation: Option<String>,
        #[arg(long)]
        json: bool,
    },
    DraftUpdate {
        #[arg(long)]
        conversation: Option<String>,
        #[arg(long)]
        message: String,
        #[arg(long = "attachment-id")]
        attachment_ids: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    DraftClear {
        #[arg(long)]
        conversation: Option<String>,
        #[arg(long)]
        json: bool,
    },
    Export {
        #[arg(long)]
        conversation: String,
        #[arg(long, value_enum, default_value = "json")]
        format: ExportFormatArg,
        #[arg(long)]
        json: bool,
    },
    Import {
        #[arg(long)]
        content: Option<String>,
        #[arg(long)]
        path: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum MessageCommand {
    Copy {
        #[arg(long)]
        conversation: String,
        #[arg(long)]
        message: String,
        #[arg(long)]
        json: bool,
    },
    Edit {
        #[arg(long)]
        conversation: String,
        #[arg(long)]
        message: String,
        #[arg(long)]
        content: String,
        #[arg(long)]
        json: bool,
    },
    Delete {
        #[arg(long)]
        conversation: String,
        #[arg(long)]
        message: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum AttachmentCommand {
    Import {
        #[arg(long)]
        conversation: String,
        #[arg(long)]
        path: PathBuf,
        #[arg(long)]
        json: bool,
    },
    ImportText {
        #[arg(long)]
        conversation: String,
        #[arg(long)]
        path: PathBuf,
        #[arg(long)]
        json: bool,
    },
    List {
        #[arg(long)]
        conversation: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum ServerCommand {
    Configure {
        #[arg(long)]
        model_path: Option<PathBuf>,
        #[arg(long)]
        slots: Option<u32>,
        #[arg(long)]
        memory_budget_mib: Option<u64>,
        #[arg(long)]
        json: bool,
    },
    Status {
        #[arg(long)]
        json: bool,
    },
    Start {
        #[arg(long)]
        json: bool,
    },
    Stop {
        #[arg(long)]
        json: bool,
    },
    Slots {
        #[arg(long)]
        json: bool,
    },
    SlotLoad {
        #[arg(long)]
        slot: usize,
        #[arg(long)]
        model_path: PathBuf,
        #[arg(long)]
        json: bool,
    },
    SlotUnload {
        #[arg(long)]
        slot: usize,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Clone, ValueEnum)]
enum ExportFormatArg {
    Json,
    Markdown,
}

#[derive(Debug, Subcommand)]
enum SettingsCommand {
    Get {
        #[arg(long)]
        json: bool,
    },
    Reset {
        #[arg(long)]
        json: bool,
    },
    Update {
        #[arg(long)]
        model_path: Option<PathBuf>,
        #[arg(long)]
        mmproj_path: Option<PathBuf>,
        #[arg(long, value_enum)]
        device: Option<NativeDeviceArg>,
        #[arg(long)]
        context_tokens: Option<u32>,
        #[arg(long)]
        batch_tokens: Option<u32>,
        #[arg(long)]
        max_parallel_sequences: Option<u32>,
        #[arg(long)]
        memory_budget_mib: Option<u64>,
        #[arg(long)]
        temperature: Option<f32>,
        #[arg(long = "top-p")]
        top_p: Option<f32>,
        #[arg(long = "max-tokens")]
        max_tokens: Option<u32>,
        #[arg(long, value_enum)]
        kv_cache_policy: Option<KvCachePolicyArg>,
        #[arg(long = "set", value_parser = parse_setting_pair)]
        set: Vec<(String, Value)>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum McpCommand {
    Status {
        #[arg(long)]
        json: bool,
    },
    Configure {
        #[arg(long)]
        name: String,
        #[arg(long)]
        command: PathBuf,
        #[arg(long = "arg")]
        args: Vec<String>,
        #[arg(long, default_value_t = true)]
        enabled: bool,
        #[arg(long)]
        json: bool,
    },
    ListServers {
        #[arg(long)]
        json: bool,
    },
    ListTools {
        #[arg(long)]
        server: String,
        #[arg(long)]
        json: bool,
    },
    ListResources {
        #[arg(long)]
        server: String,
        #[arg(long)]
        json: bool,
    },
    ReadResource {
        #[arg(long)]
        server: String,
        #[arg(long)]
        uri: String,
        #[arg(long)]
        json: bool,
    },
    ListPrompts {
        #[arg(long)]
        server: String,
        #[arg(long)]
        json: bool,
    },
    GetPrompt {
        #[arg(long)]
        server: String,
        #[arg(long)]
        prompt: String,
        #[arg(long, value_parser = parse_json_value, default_value = "{}")]
        arguments: Value,
        #[arg(long)]
        json: bool,
    },
    CallTool {
        #[arg(long)]
        server: String,
        #[arg(long)]
        tool: String,
        #[arg(long, value_parser = parse_json_value, default_value = "{}")]
        arguments: Value,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum ToolLoopCommand {
    Run {
        #[arg(long)]
        conversation: String,
        #[arg(long)]
        prompt: String,
        #[arg(long)]
        server: String,
        #[arg(long)]
        tool: String,
        #[arg(long, value_parser = parse_json_value, default_value = "{}")]
        arguments: Value,
        #[arg(long, default_value_t = 1)]
        max_turns: u32,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Clone, ValueEnum)]
enum KvCachePolicyArg {
    None,
    PromptPrefix,
    KvCacheCandidate,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum NativeDeviceArg {
    Auto,
    Cpu,
    Metal,
}

#[derive(Debug, Subcommand)]
enum SkillCommand {
    Create {
        #[arg(long)]
        name: String,
        #[arg(long, default_value = "")]
        description: String,
        #[arg(long)]
        prompt_template: String,
        #[arg(long, default_value = "Use this prompt before the next answer.")]
        usage_hint: String,
        #[arg(long, value_enum, default_value = "none")]
        cache_policy: KvCachePolicyArg,
        #[arg(long)]
        json: bool,
    },
    List {
        #[arg(long)]
        json: bool,
    },
    Edit {
        #[arg(long)]
        skill: String,
        #[arg(long)]
        name: String,
        #[arg(long, default_value = "")]
        description: String,
        #[arg(long)]
        prompt_template: String,
        #[arg(long, default_value = "Use this prompt before the next answer.")]
        usage_hint: String,
        #[arg(long, value_enum, default_value = "none")]
        cache_policy: KvCachePolicyArg,
        #[arg(long)]
        json: bool,
    },
    Apply {
        #[arg(long)]
        conversation: String,
        #[arg(long)]
        skill: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum KvCacheCommand {
    Status {
        #[arg(long)]
        json: bool,
    },
    Save {
        #[arg(long)]
        skill: Option<String>,
        #[arg(long)]
        json: bool,
    },
    Restore {
        #[arg(long)]
        cache: Option<String>,
        #[arg(long)]
        json: bool,
    },
    Clear {
        #[arg(long)]
        json: bool,
    },
}

fn main() -> Result<()> {
    let result = run();
    mom_llama_runtime::unload_resident_model();
    result
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Engine { command } => match command {
            EngineCommand::Check { json } => print_result(
                mom_llama_runtime::engine_check(EngineCheckOptions::default())?,
                json,
            ),
            EngineCommand::Configure {
                model_path,
                device,
                context_tokens,
                batch_tokens,
                max_parallel_sequences,
                memory_budget_mib,
                json,
            } => print_result(
                mom_llama_runtime::configure_engine(
                    model_path,
                    device.map(map_native_device),
                    context_tokens,
                    batch_tokens,
                    max_parallel_sequences,
                    memory_budget_mib.map(mib_to_bytes),
                )?,
                json,
            ),
        },
        Command::Model { command } => match command {
            ModelCommand::List { json } => print_result(mom_llama_runtime::model_list()?, json),
            ModelCommand::Select { model_path, json } => {
                print_result(mom_llama_runtime::model_select(model_path)?, json)
            }
            ModelCommand::Status { json } => {
                print_result(mom_llama_runtime::model_slot_list()?, json)
            }
            ModelCommand::Load {
                slot,
                model_path,
                json,
            } => print_result(mom_llama_runtime::model_slot_load(slot, model_path)?, json),
            ModelCommand::Unload { slot, json } => {
                print_result(mom_llama_runtime::model_slot_unload(slot)?, json)
            }
        },
        Command::Chat { command } => match command {
            ChatCommand::Send {
                conversation,
                message,
                timeout_s,
                stream_jsonl,
                json,
            } => {
                let options = ChatSendOptions {
                    timeout_s: timeout_s.unwrap_or_else(|| ChatSendOptions::default().timeout_s),
                    fake_fixture: false,
                };
                if stream_jsonl {
                    let result = mom_llama_runtime::chat_send_stream(
                        ChatSendInput {
                            conversation_id: conversation,
                            message,
                        },
                        options,
                        |event| {
                            println!("{}", serde_json::to_string(&event)?);
                            Ok(())
                        },
                    )?;
                    print_json_line_result(result)
                } else {
                    print_result(
                        mom_llama_runtime::chat_send(
                            ChatSendInput {
                                conversation_id: conversation,
                                message,
                            },
                            options,
                        )?,
                        json,
                    )
                }
            }
            ChatCommand::Cancel { conversation, json } => {
                print_result(mom_llama_runtime::chat_cancel(&conversation)?, json)
            }
            ChatCommand::Regenerate {
                conversation,
                timeout_s,
                json,
            } => print_result(
                mom_llama_runtime::chat_regenerate(
                    &conversation,
                    ChatSendOptions {
                        timeout_s: timeout_s
                            .unwrap_or_else(|| ChatSendOptions::default().timeout_s),
                        fake_fixture: false,
                    },
                )?,
                json,
            ),
            ChatCommand::Continue {
                conversation,
                timeout_s,
                json,
            } => print_result(
                mom_llama_runtime::chat_continue(
                    &conversation,
                    ChatSendOptions {
                        timeout_s: timeout_s
                            .unwrap_or_else(|| ChatSendOptions::default().timeout_s),
                        fake_fixture: false,
                    },
                )?,
                json,
            ),
        },
        Command::Consult { command } => match command {
            ConsultCommand::PanelList { json } => {
                print_result(mom_llama_runtime::consult_panel_list()?, json)
            }
            ConsultCommand::Start {
                conversation,
                prompt,
                panel,
                timeout_s,
                stream_jsonl,
                json,
            } => {
                let input = ConsultStartInput {
                    conversation_id: conversation,
                    prompt,
                    panel_id: panel,
                };
                let options = ConsultStartOptions {
                    timeout_s: timeout_s
                        .unwrap_or_else(|| ConsultStartOptions::default().timeout_s),
                    fake_fixture: false,
                };
                if stream_jsonl {
                    let result = mom_llama_runtime::consult_start_stream(
                        input,
                        options,
                        Some(|event| {
                            println!("{}", serde_json::to_string(&event)?);
                            Ok(())
                        }),
                    )?;
                    print_json_line_result(result)
                } else {
                    print_result(mom_llama_runtime::consult_start(input, options)?, json)
                }
            }
            ConsultCommand::Status { run, json } => {
                print_result(mom_llama_runtime::consult_status(&run)?, json)
            }
            ConsultCommand::Cancel { run, seat, json } => print_result(
                mom_llama_runtime::consult_cancel(&run, seat.as_deref())?,
                json,
            ),
            ConsultCommand::Synthesize { run, seats, json } => {
                print_result(mom_llama_runtime::consult_synthesize(&run, seats)?, json)
            }
        },
        Command::Message { command } => match command {
            MessageCommand::Copy {
                conversation,
                message,
                json,
            } => print_result(
                mom_llama_runtime::message_copy(&conversation, &message)?,
                json,
            ),
            MessageCommand::Edit {
                conversation,
                message,
                content,
                json,
            } => print_result(
                mom_llama_runtime::message_edit(&conversation, &message, content)?,
                json,
            ),
            MessageCommand::Delete {
                conversation,
                message,
                json,
            } => print_result(
                mom_llama_runtime::message_delete(&conversation, &message)?,
                json,
            ),
        },
        Command::Attachment { command } => match command {
            AttachmentCommand::Import {
                conversation,
                path,
                json,
            } => print_result(
                mom_llama_runtime::attachment_import(&conversation, &path)?,
                json,
            ),
            AttachmentCommand::ImportText {
                conversation,
                path,
                json,
            } => print_result(
                mom_llama_runtime::text_attachment_import(&conversation, &path)?,
                json,
            ),
            AttachmentCommand::List { conversation, json } => print_result(
                mom_llama_runtime::attachment_list(conversation.as_deref())?,
                json,
            ),
        },
        Command::Server { command } => match command {
            ServerCommand::Configure {
                model_path,
                slots,
                memory_budget_mib,
                json,
            } => print_result(
                mom_llama_runtime::server_configure(
                    model_path,
                    slots,
                    memory_budget_mib.map(mib_to_bytes),
                )?,
                json,
            ),
            ServerCommand::Status { json } => {
                print_result(mom_llama_runtime::server_status()?, json)
            }
            ServerCommand::Start { json } => print_result(mom_llama_runtime::server_start()?, json),
            ServerCommand::Stop { json } => print_result(mom_llama_runtime::server_stop()?, json),
            ServerCommand::Slots { json } => {
                print_result(mom_llama_runtime::model_slot_list()?, json)
            }
            ServerCommand::SlotLoad {
                slot,
                model_path,
                json,
            } => print_result(mom_llama_runtime::model_slot_load(slot, model_path)?, json),
            ServerCommand::SlotUnload { slot, json } => {
                print_result(mom_llama_runtime::model_slot_unload(slot)?, json)
            }
        },
        Command::Conversation { command } => match command {
            ConversationCommand::New { title, json } => {
                print_result(mom_llama_runtime::conversation_new(title)?, json)
            }
            ConversationCommand::List { json } => {
                print_result(mom_llama_runtime::conversation_list()?, json)
            }
            ConversationCommand::Select { conversation, json } => {
                print_result(mom_llama_runtime::conversation_select(&conversation)?, json)
            }
            ConversationCommand::Search { query, json } => {
                print_result(mom_llama_runtime::conversation_search(&query)?, json)
            }
            ConversationCommand::Rename {
                conversation,
                title,
                json,
            } => print_result(
                mom_llama_runtime::conversation_rename(&conversation, title)?,
                json,
            ),
            ConversationCommand::Delete { conversation, json } => {
                print_result(mom_llama_runtime::conversation_delete(&conversation)?, json)
            }
            ConversationCommand::Fork {
                conversation,
                message,
                json,
            } => print_result(
                mom_llama_runtime::conversation_fork(&conversation, &message)?,
                json,
            ),
            ConversationCommand::Siblings { conversation, json } => print_result(
                mom_llama_runtime::conversation_siblings(&conversation)?,
                json,
            ),
            ConversationCommand::DraftGet { conversation, json } => {
                print_result(mom_llama_runtime::draft_get(conversation.as_deref())?, json)
            }
            ConversationCommand::DraftUpdate {
                conversation,
                message,
                attachment_ids,
                json,
            } => print_result(
                mom_llama_runtime::draft_update(conversation.as_deref(), message, attachment_ids)?,
                json,
            ),
            ConversationCommand::DraftClear { conversation, json } => print_result(
                mom_llama_runtime::draft_clear(conversation.as_deref())?,
                json,
            ),
            ConversationCommand::Export {
                conversation,
                format,
                json,
            } => print_result(
                mom_llama_runtime::conversation_export(
                    &conversation,
                    match format {
                        ExportFormatArg::Json => ConversationExportFormat::Json,
                        ExportFormatArg::Markdown => ConversationExportFormat::Markdown,
                    },
                )?,
                json,
            ),
            ConversationCommand::Import {
                content,
                path,
                json,
            } => {
                let content = match (content, path) {
                    (Some(content), _) => content,
                    (None, Some(path)) => std::fs::read_to_string(path)?,
                    (None, None) => String::new(),
                };
                print_result(mom_llama_runtime::conversation_import_json(&content)?, json)
            }
        },
        Command::Settings { command } => match command {
            SettingsCommand::Get { json } => print_result(mom_llama_runtime::settings_get()?, json),
            SettingsCommand::Reset { json } => {
                print_result(mom_llama_runtime::settings_reset()?, json)
            }
            SettingsCommand::Update {
                model_path,
                mmproj_path,
                device,
                context_tokens,
                batch_tokens,
                max_parallel_sequences,
                memory_budget_mib,
                temperature,
                top_p,
                max_tokens,
                kv_cache_policy,
                set,
                json,
            } => print_result(
                mom_llama_runtime::settings_update(SettingsUpdate {
                    model_path,
                    mmproj_path,
                    native_device: device.map(map_native_device),
                    context_tokens,
                    batch_tokens,
                    max_parallel_sequences,
                    resident_memory_budget_bytes: memory_budget_mib.map(mib_to_bytes),
                    temperature,
                    top_p,
                    max_tokens,
                    kv_cache_policy: kv_cache_policy.map(map_cache_policy),
                    upstream_settings: (!set.is_empty()).then(|| set.into_iter().collect()),
                })?,
                json,
            ),
        },
        Command::Mcp { command } => match command {
            McpCommand::Status { json } => print_result(mom_llama_runtime::mcp_status()?, json),
            McpCommand::Configure {
                name,
                command,
                args,
                enabled,
                json,
            } => print_result(
                mom_llama_runtime::mcp_configure(name, command, args, enabled)?,
                json,
            ),
            McpCommand::ListServers { json } => {
                print_result(mom_llama_runtime::mcp_list_servers()?, json)
            }
            McpCommand::ListTools { server, json } => {
                print_result(mom_llama_runtime::mcp_list_tools(&server)?, json)
            }
            McpCommand::ListResources { server, json } => {
                print_result(mom_llama_runtime::mcp_list_resources(&server)?, json)
            }
            McpCommand::ReadResource { server, uri, json } => {
                print_result(mom_llama_runtime::mcp_read_resource(&server, &uri)?, json)
            }
            McpCommand::ListPrompts { server, json } => {
                print_result(mom_llama_runtime::mcp_list_prompts(&server)?, json)
            }
            McpCommand::GetPrompt {
                server,
                prompt,
                arguments,
                json,
            } => print_result(
                mom_llama_runtime::mcp_get_prompt(&server, &prompt, arguments)?,
                json,
            ),
            McpCommand::CallTool {
                server,
                tool,
                arguments,
                json,
            } => print_result(
                mom_llama_runtime::mcp_call_tool(&server, &tool, arguments)?,
                json,
            ),
        },
        Command::ToolLoop { command } => match command {
            ToolLoopCommand::Run {
                conversation,
                prompt,
                server,
                tool,
                arguments,
                max_turns,
                json,
            } => print_result(
                mom_llama_runtime::tool_loop_run(
                    &conversation,
                    prompt,
                    server,
                    tool,
                    arguments,
                    max_turns,
                )?,
                json,
            ),
        },
        Command::Skill { command } => match command {
            SkillCommand::Create {
                name,
                description,
                prompt_template,
                usage_hint,
                cache_policy,
                json,
            } => print_result(
                mom_llama_runtime::skill_store::skill_create(
                    name,
                    description,
                    prompt_template,
                    usage_hint,
                    map_cache_policy(cache_policy),
                )?,
                json,
            ),
            SkillCommand::List { json } => {
                print_result(mom_llama_runtime::skill_store::skill_list()?, json)
            }
            SkillCommand::Edit {
                skill,
                name,
                description,
                prompt_template,
                usage_hint,
                cache_policy,
                json,
            } => print_result(
                mom_llama_runtime::skill_store::skill_update(
                    &skill,
                    name,
                    description,
                    prompt_template,
                    usage_hint,
                    map_cache_policy(cache_policy),
                )?,
                json,
            ),
            SkillCommand::Apply {
                conversation,
                skill,
                json,
            } => print_result(
                mom_llama_runtime::skill_store::skill_apply(&conversation, &skill)?,
                json,
            ),
        },
        Command::KvCache { command } => match command {
            KvCacheCommand::Status { json } => {
                print_result(mom_llama_runtime::kv_cache_status()?, json)
            }
            KvCacheCommand::Save { skill, json } => {
                print_result(mom_llama_runtime::kv_cache_save(skill)?, json)
            }
            KvCacheCommand::Restore { cache, json } => {
                print_result(mom_llama_runtime::kv_cache_restore(cache)?, json)
            }
            KvCacheCommand::Clear { json } => {
                print_result(mom_llama_runtime::kv_cache_clear()?, json)
            }
        },
    }
}

fn map_cache_policy(arg: KvCachePolicyArg) -> KvCachePolicy {
    match arg {
        KvCachePolicyArg::None => KvCachePolicy::None,
        KvCachePolicyArg::PromptPrefix => KvCachePolicy::PromptPrefix,
        KvCachePolicyArg::KvCacheCandidate => KvCachePolicy::KvCacheCandidate,
    }
}

fn map_native_device(arg: NativeDeviceArg) -> NativeDevice {
    match arg {
        NativeDeviceArg::Auto => NativeDevice::Auto,
        NativeDeviceArg::Cpu => NativeDevice::Cpu,
        NativeDeviceArg::Metal => NativeDevice::Metal,
    }
}

fn mib_to_bytes(value: u64) -> u64 {
    value.saturating_mul(1024 * 1024)
}

fn parse_setting_pair(raw: &str) -> std::result::Result<(String, Value), String> {
    let Some((key, raw_value)) = raw.split_once('=') else {
        return Err("settings --set expects key=value".to_string());
    };
    let key = key.trim();
    if key.is_empty() {
        return Err("settings --set key cannot be empty".to_string());
    }
    let value = serde_json::from_str(raw_value)
        .unwrap_or_else(|_| Value::String(raw_value.trim().to_string()));
    Ok((key.to_string(), value))
}

fn parse_json_value(raw: &str) -> std::result::Result<Value, String> {
    serde_json::from_str(raw).map_err(|error| error.to_string())
}

fn print_result<T>(result: T, json_output: bool) -> Result<()>
where
    T: Serialize,
{
    let _ = json_output;
    mom_llama_runtime::persist_command_receipt(&result)?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

fn print_json_line_result<T>(result: T) -> Result<()>
where
    T: Serialize,
{
    mom_llama_runtime::persist_command_receipt(&result)?;
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}
