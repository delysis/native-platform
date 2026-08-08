use crate::config::resolve_settings;
use crate::now_ms;
use crate::receipts::{Blocker, CommandResult};
use crate::store::RuntimeStore;
use anyhow::Result;
use llama_native_types::SamplingConfig;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

const CONVERSATIONS_FILE: &str = "conversations.json";
const DRAFTS_FILE: &str = "drafts.json";
const CONVERSATIONS_NAMESPACE: &str = "conversations.v2";
const DRAFTS_NAMESPACE: &str = "drafts.v2";
const MAX_TEXT_ATTACHMENT_BYTES: u64 = 256 * 1024;
const NEW_CHAT_DRAFT_KEY: &str = "__new_chat__";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConversationKind {
    #[default]
    Chat,
    PersonaTemplate,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "template", rename_all = "snake_case")]
pub enum ChatTemplatePolicy {
    #[default]
    ModelDefault,
    FrozenSource(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolBinding {
    pub server: String,
    pub tool: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConversationExecutionProfile {
    #[serde(default)]
    pub mention_handle: String,
    #[serde(default)]
    pub model_path: Option<PathBuf>,
    #[serde(default)]
    pub mmproj_path: Option<PathBuf>,
    #[serde(default)]
    pub system_message: Option<String>,
    #[serde(default)]
    pub sampling: Option<SamplingConfig>,
    #[serde(default)]
    pub chat_template: ChatTemplatePolicy,
    #[serde(default)]
    pub tool_bindings: Vec<ToolBinding>,
    #[serde(default = "default_source_history_tokens")]
    pub source_history_tokens: u32,
    #[serde(default = "default_host_context_tokens")]
    pub host_context_tokens: u32,
    #[serde(default = "default_profile_version")]
    pub version: u64,
}

impl Default for ConversationExecutionProfile {
    fn default() -> Self {
        Self {
            mention_handle: String::new(),
            model_path: None,
            mmproj_path: None,
            system_message: None,
            sampling: None,
            chat_template: ChatTemplatePolicy::ModelDefault,
            tool_bindings: Vec::new(),
            source_history_tokens: default_source_history_tokens(),
            host_context_tokens: default_host_context_tokens(),
            version: default_profile_version(),
        }
    }
}

const fn default_source_history_tokens() -> u32 {
    4096
}

const fn default_host_context_tokens() -> u32 {
    2048
}

const fn default_profile_version() -> u64 {
    1
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageSpeakerKind {
    Persona,
    LiveChat,
    Synthesis,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageAttribution {
    pub kind: MessageSpeakerKind,
    pub source_id: String,
    pub handle: String,
    pub label: String,
    pub version: u64,
    pub invocation_id: String,
    pub target_order: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Message {
    pub id: String,
    pub conversation_id: String,
    pub role: MessageRole,
    pub content: String,
    pub created_at: String,
    pub parent_id: Option<String>,
    pub model: Option<String>,
    pub receipt_id: Option<String>,
    pub prompt_tokens: Option<usize>,
    pub completion_tokens: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(default)]
    pub reasoning_incomplete: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_index: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribution: Option<MessageAttribution>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Conversation {
    pub id: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub kind: ConversationKind,
    #[serde(default)]
    pub execution_profile: ConversationExecutionProfile,
    pub selected_model_path: Option<PathBuf>,
    #[serde(default)]
    pub source_conversation_id: Option<String>,
    #[serde(default)]
    pub source_message_id: Option<String>,
    #[serde(default)]
    pub branch_root_message_id: Option<String>,
    #[serde(default)]
    pub active_leaf_message_id: Option<String>,
    #[serde(default)]
    pub current_skill_ids: Vec<String>,
    #[serde(default)]
    pub messages: Vec<Message>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct ConversationDb {
    #[serde(default)]
    pub conversations: Vec<Conversation>,
    pub selected_conversation_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversationExportFormat {
    Json,
    Markdown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConversationExport {
    pub conversation_id: String,
    pub format: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConversationSearchHit {
    pub conversation_id: String,
    pub title: String,
    pub snippet: String,
    pub message_count: usize,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConversationMutation {
    pub conversation_id: String,
    pub changed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConversationBranchSibling {
    pub conversation_id: String,
    pub title: String,
    pub message_count: usize,
    pub updated_at: String,
    pub selected: bool,
    pub source_conversation_id: Option<String>,
    pub source_message_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageBranchSibling {
    pub message_id: String,
    pub parent_id: Option<String>,
    pub role: MessageRole,
    pub preview: String,
    pub created_at: String,
    pub selected: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageBranchSet {
    pub conversation_id: String,
    pub parent_id: Option<String>,
    pub active_message_id: String,
    pub siblings: Vec<MessageBranchSibling>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct DraftDb {
    #[serde(default)]
    pub drafts: Vec<DraftMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DraftMessage {
    pub conversation_id: Option<String>,
    pub message: String,
    #[serde(default)]
    pub attachment_ids: Vec<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageCopy {
    pub conversation_id: String,
    pub message_id: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TextAttachmentImport {
    pub conversation_id: String,
    pub message_id: String,
    pub file_name: String,
    pub bytes: u64,
}

pub fn conversation_new(title: Option<String>) -> Result<CommandResult<Conversation>> {
    let mut db = load_db()?;
    let now = now_ms().to_string();
    let settings = resolve_settings()?;
    let id = Uuid::new_v4().to_string();
    let title = title.unwrap_or_else(|| "New chat".to_string());
    let mention_handle = new_chat_handle(&title, &id);
    let conversation = Conversation {
        id,
        title,
        created_at: now.clone(),
        updated_at: now,
        kind: ConversationKind::Chat,
        execution_profile: ConversationExecutionProfile {
            mention_handle,
            model_path: settings.model_path.clone(),
            mmproj_path: settings.mmproj_path.clone(),
            sampling: Some(settings.sampling_config()),
            ..ConversationExecutionProfile::default()
        },
        selected_model_path: settings.model_path,
        source_conversation_id: None,
        source_message_id: None,
        branch_root_message_id: None,
        active_leaf_message_id: None,
        current_skill_ids: Vec::new(),
        messages: Vec::new(),
    };
    db.selected_conversation_id = Some(conversation.id.clone());
    db.conversations.insert(0, conversation.clone());
    let path = save_db(&db)?;
    Ok(CommandResult::passed(
        "mom_llama.conversation_new",
        "contracted",
        conversation,
        vec![path.display().to_string()],
        Vec::new(),
        false,
        false,
    ))
}

fn new_chat_handle(title: &str, id: &str) -> String {
    let mut base = title
        .chars()
        .flat_map(char::to_lowercase)
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    while base.contains("--") {
        base = base.replace("--", "-");
    }
    let base = base.trim_matches('-');
    let base = if base.len() < 2 { "chat" } else { base };
    let suffix = id
        .chars()
        .filter(|character| *character != '-')
        .take(6)
        .collect::<String>();
    let keep = 48usize.saturating_sub(suffix.len() + 1);
    format!("{}-{suffix}", base.chars().take(keep).collect::<String>())
}

pub fn conversation_list() -> Result<CommandResult<Vec<Conversation>>> {
    let db = load_db()?;
    Ok(CommandResult::passed(
        "mom_llama.conversation_list",
        "contracted",
        db.conversations,
        Vec::new(),
        Vec::new(),
        false,
        false,
    ))
}

pub fn conversation_select(id: &str) -> Result<CommandResult<Conversation>> {
    let mut db = load_db()?;
    let Some(conversation) = db
        .conversations
        .iter()
        .find(|conversation| conversation.id == id)
        .cloned()
    else {
        return Ok(CommandResult::blocked(
            "mom_llama.conversation_select",
            "stub_blocked",
            Blocker::new(
                "conversation_not_found",
                format!("Conversation {id} was not found."),
                vec!["Run `mom-llama conversation list --json`.".to_string()],
            ),
        ));
    };
    db.selected_conversation_id = Some(id.to_string());
    let path = save_db(&db)?;
    Ok(CommandResult::passed(
        "mom_llama.conversation_select",
        "contracted",
        project_conversation(&conversation),
        vec![path.display().to_string()],
        Vec::new(),
        false,
        false,
    ))
}

pub fn conversation_search(query: &str) -> Result<CommandResult<Vec<ConversationSearchHit>>> {
    let db = load_db()?;
    let query = query.trim().to_lowercase();
    let hits = db
        .conversations
        .iter()
        .filter_map(|conversation| {
            let title_matches =
                query.is_empty() || conversation.title.to_lowercase().contains(&query);
            let message_match = conversation.messages.iter().find(|message| {
                query.is_empty() || message.content.to_lowercase().contains(&query)
            });
            if !title_matches && message_match.is_none() {
                return None;
            }
            let snippet = message_match
                .map(|message| snippet(&message.content, &query))
                .unwrap_or_else(|| conversation.title.clone());
            Some(ConversationSearchHit {
                conversation_id: conversation.id.clone(),
                title: conversation.title.clone(),
                snippet,
                message_count: conversation.messages.len(),
                updated_at: conversation.updated_at.clone(),
            })
        })
        .collect::<Vec<_>>();
    Ok(CommandResult::passed(
        "mom_llama.conversation_search",
        "contracted",
        hits,
        Vec::new(),
        Vec::new(),
        false,
        false,
    ))
}

pub fn conversation_rename(id: &str, title: String) -> Result<CommandResult<Conversation>> {
    let mut db = load_db()?;
    let Some(conversation) = db
        .conversations
        .iter_mut()
        .find(|conversation| conversation.id == id)
    else {
        return Ok(conversation_not_found("mom_llama.conversation_rename", id));
    };
    conversation.title = title.trim().to_string();
    if conversation.title.is_empty() {
        conversation.title = "Untitled conversation".to_string();
    }
    conversation.updated_at = now_ms().to_string();
    let result = conversation.clone();
    let path = save_db(&db)?;
    Ok(CommandResult::passed(
        "mom_llama.conversation_rename",
        "contracted",
        result,
        vec![path.display().to_string()],
        Vec::new(),
        false,
        false,
    ))
}

pub fn conversation_system_message_update(
    id: &str,
    system_message: Option<String>,
) -> Result<CommandResult<Conversation>> {
    let (db, mut conversation) = get_or_create_conversation(id)?;
    let system_message = system_message
        .map(|message| message.trim().to_string())
        .filter(|message| !message.is_empty());
    if conversation.execution_profile.system_message == system_message {
        return Ok(CommandResult::passed(
            "mom_llama.conversation_system_message_update",
            "contracted",
            project_conversation(&conversation),
            Vec::new(),
            vec!["conversation instructions unchanged".to_string()],
            false,
            false,
        ));
    }
    conversation.execution_profile.system_message = system_message;
    conversation.execution_profile.version =
        conversation.execution_profile.version.saturating_add(1);
    conversation.updated_at = now_ms().to_string();
    let path = upsert_conversation(db, conversation.clone())?;
    Ok(CommandResult::passed(
        "mom_llama.conversation_system_message_update",
        "contracted",
        project_conversation(&conversation),
        vec![path.display().to_string()],
        vec!["conversation-scoped instructions; blank inherits the app default".to_string()],
        false,
        false,
    ))
}

pub fn conversation_delete(id: &str) -> Result<CommandResult<ConversationMutation>> {
    let mut db = load_db()?;
    let before = db.conversations.len();
    db.conversations
        .retain(|conversation| conversation.id != id);
    let changed = before != db.conversations.len();
    if !changed {
        return Ok(conversation_not_found("mom_llama.conversation_delete", id));
    }
    if db.selected_conversation_id.as_deref() == Some(id) {
        db.selected_conversation_id = db
            .conversations
            .first()
            .map(|conversation| conversation.id.clone());
    }
    let path = save_db(&db)?;
    Ok(CommandResult::passed(
        "mom_llama.conversation_delete",
        "contracted",
        ConversationMutation {
            conversation_id: id.to_string(),
            changed,
        },
        vec![path.display().to_string()],
        Vec::new(),
        false,
        false,
    ))
}

pub fn conversation_import_json(content: &str) -> Result<CommandResult<Conversation>> {
    let mut imported = serde_json::from_str::<Conversation>(content)?;
    let mut db = load_db()?;
    if imported.id.trim().is_empty()
        || db
            .conversations
            .iter()
            .any(|conversation| conversation.id == imported.id)
    {
        imported.id = Uuid::new_v4().to_string();
        for message in &mut imported.messages {
            message.conversation_id = imported.id.clone();
        }
    }
    imported.updated_at = now_ms().to_string();
    db.selected_conversation_id = Some(imported.id.clone());
    db.conversations.insert(0, imported.clone());
    let path = save_db(&db)?;
    Ok(CommandResult::passed(
        "mom_llama.conversation_import",
        "contracted",
        imported,
        vec![path.display().to_string()],
        Vec::new(),
        false,
        false,
    ))
}

pub fn conversation_export(
    id: &str,
    format: ConversationExportFormat,
) -> Result<CommandResult<ConversationExport>> {
    let db = load_db()?;
    let Some(conversation) = db
        .conversations
        .iter()
        .find(|conversation| conversation.id == id)
    else {
        return Ok(CommandResult::blocked(
            "mom_llama.conversation_export",
            "stub_blocked",
            Blocker::new(
                "conversation_not_found",
                format!("Conversation {id} was not found."),
                vec!["Run `mom-llama conversation list --json`.".to_string()],
            ),
        ));
    };
    let (format_name, content) = match format {
        ConversationExportFormat::Json => (
            "json".to_string(),
            serde_json::to_string_pretty(conversation)?,
        ),
        ConversationExportFormat::Markdown => {
            let mut lines = vec![format!("# {}", conversation.title)];
            for message in &conversation.messages {
                lines.push(String::new());
                lines.push(format!("## {:?}", message.role));
                lines.push(message.content.clone());
            }
            ("markdown".to_string(), lines.join("\n"))
        }
    };
    Ok(CommandResult::passed(
        "mom_llama.conversation_export",
        "contracted",
        ConversationExport {
            conversation_id: id.to_string(),
            format: format_name,
            content,
        },
        Vec::new(),
        Vec::new(),
        false,
        false,
    ))
}

pub fn message_edit(
    conversation_id: &str,
    message_id: &str,
    content: String,
) -> Result<CommandResult<Message>> {
    let mut db = load_db()?;
    let Some(conversation) = db
        .conversations
        .iter_mut()
        .find(|conversation| conversation.id == conversation_id)
    else {
        return Ok(conversation_not_found(
            "mom_llama.message_edit",
            conversation_id,
        ));
    };
    let Some(message) = conversation
        .messages
        .iter()
        .find(|message| message.id == message_id)
        .cloned()
    else {
        return Ok(message_not_found("mom_llama.message_edit", message_id));
    };
    let mut edited = message;
    edited.id = Uuid::new_v4().to_string();
    edited.content = content;
    edited.created_at = now_ms().to_string();
    edited.receipt_id = None;
    edited.branch_index = None;
    edited.branch_count = None;
    conversation.active_leaf_message_id = Some(edited.id.clone());
    conversation.messages.push(edited.clone());
    conversation.updated_at = now_ms().to_string();
    if conversation.kind == ConversationKind::PersonaTemplate {
        conversation.execution_profile.version =
            conversation.execution_profile.version.saturating_add(1);
    }
    let persona_version =
        (conversation.kind == ConversationKind::PersonaTemplate).then(|| conversation.clone());
    let path = save_db(&db)?;
    if let Some(persona) = persona_version {
        crate::personas::record_persona_version(&persona)?;
    }
    Ok(CommandResult::passed(
        "mom_llama.message_edit",
        "contracted",
        edited,
        vec![path.display().to_string()],
        Vec::new(),
        false,
        false,
    ))
}

pub fn message_delete(
    conversation_id: &str,
    message_id: &str,
) -> Result<CommandResult<ConversationMutation>> {
    let mut db = load_db()?;
    let Some(conversation) = db
        .conversations
        .iter_mut()
        .find(|conversation| conversation.id == conversation_id)
    else {
        return Ok(conversation_not_found(
            "mom_llama.message_delete",
            conversation_id,
        ));
    };
    let Some(parent_id) = conversation
        .messages
        .iter()
        .find(|message| message.id == message_id)
        .map(|message| message.parent_id.clone())
    else {
        return Ok(message_not_found("mom_llama.message_delete", message_id));
    };
    let removed = descendant_ids(conversation, message_id);
    let before = conversation.messages.len();
    conversation
        .messages
        .retain(|message| !removed.contains(&message.id));
    let changed = before != conversation.messages.len();
    if !changed {
        return Ok(message_not_found("mom_llama.message_delete", message_id));
    }
    if conversation
        .active_leaf_message_id
        .as_ref()
        .is_some_and(|active| removed.contains(active))
    {
        conversation.active_leaf_message_id = parent_id;
    }
    conversation.updated_at = now_ms().to_string();
    if conversation.kind == ConversationKind::PersonaTemplate {
        conversation.execution_profile.version =
            conversation.execution_profile.version.saturating_add(1);
    }
    let path = save_db(&db)?;
    Ok(CommandResult::passed(
        "mom_llama.message_delete",
        "contracted",
        ConversationMutation {
            conversation_id: conversation_id.to_string(),
            changed,
        },
        vec![path.display().to_string()],
        Vec::new(),
        false,
        false,
    ))
}

pub fn message_copy(conversation_id: &str, message_id: &str) -> Result<CommandResult<MessageCopy>> {
    let db = load_db()?;
    let Some(conversation) = db
        .conversations
        .iter()
        .find(|conversation| conversation.id == conversation_id)
    else {
        return Ok(conversation_not_found(
            "mom_llama.message_copy",
            conversation_id,
        ));
    };
    let Some(message) = conversation
        .messages
        .iter()
        .find(|message| message.id == message_id)
    else {
        return Ok(message_not_found("mom_llama.message_copy", message_id));
    };
    Ok(CommandResult::passed(
        "mom_llama.message_copy",
        "contracted",
        MessageCopy {
            conversation_id: conversation_id.to_string(),
            message_id: message_id.to_string(),
            content: message.content.clone(),
        },
        Vec::new(),
        Vec::new(),
        false,
        false,
    ))
}

pub fn conversation_fork(
    conversation_id: &str,
    message_id: &str,
) -> Result<CommandResult<Conversation>> {
    let mut db = load_db()?;
    let Some(source) = db
        .conversations
        .iter()
        .find(|conversation| conversation.id == conversation_id)
        .cloned()
    else {
        return Ok(conversation_not_found(
            "mom_llama.conversation_fork",
            conversation_id,
        ));
    };
    let Some(index) = source
        .messages
        .iter()
        .position(|message| message.id == message_id)
    else {
        return Ok(message_not_found("mom_llama.conversation_fork", message_id));
    };
    let now = now_ms().to_string();
    let fork_id = Uuid::new_v4().to_string();
    let mut messages = source.messages[..=index].to_vec();
    for message in &mut messages {
        message.conversation_id = fork_id.clone();
    }
    let fork = Conversation {
        id: fork_id.clone(),
        title: format!("{} fork", source.title),
        created_at: now.clone(),
        updated_at: now,
        kind: ConversationKind::Chat,
        execution_profile: source.execution_profile,
        selected_model_path: source.selected_model_path,
        source_conversation_id: Some(source.id.clone()),
        source_message_id: Some(message_id.to_string()),
        branch_root_message_id: Some(message_id.to_string()),
        active_leaf_message_id: messages.last().map(|message| message.id.clone()),
        current_skill_ids: source.current_skill_ids,
        messages,
    };
    db.selected_conversation_id = Some(fork_id);
    db.conversations.insert(0, fork.clone());
    let path = save_db(&db)?;
    Ok(CommandResult::passed(
        "mom_llama.conversation_fork",
        "contracted",
        fork,
        vec![path.display().to_string()],
        Vec::new(),
        false,
        false,
    ))
}

pub fn message_branches(
    conversation_id: &str,
    message_id: &str,
) -> Result<CommandResult<MessageBranchSet>> {
    let db = load_db()?;
    let Some(conversation) = db
        .conversations
        .iter()
        .find(|conversation| conversation.id == conversation_id)
    else {
        return Ok(conversation_not_found(
            "mom_llama.message_branches",
            conversation_id,
        ));
    };
    let Some(selected) = conversation
        .messages
        .iter()
        .find(|message| message.id == message_id)
    else {
        return Ok(message_not_found("mom_llama.message_branches", message_id));
    };
    let mut siblings = conversation
        .messages
        .iter()
        .filter(|message| message.parent_id == selected.parent_id && message.role == selected.role)
        .map(|message| MessageBranchSibling {
            message_id: message.id.clone(),
            parent_id: message.parent_id.clone(),
            role: message.role.clone(),
            preview: message.content.chars().take(120).collect(),
            created_at: message.created_at.clone(),
            selected: message.id == message_id,
        })
        .collect::<Vec<_>>();
    siblings.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.message_id.cmp(&right.message_id))
    });
    Ok(CommandResult::passed(
        "mom_llama.message_branches",
        "contracted",
        MessageBranchSet {
            conversation_id: conversation_id.to_string(),
            parent_id: selected.parent_id.clone(),
            active_message_id: message_id.to_string(),
            siblings,
        },
        Vec::new(),
        Vec::new(),
        false,
        false,
    ))
}

pub fn message_branch_select(
    conversation_id: &str,
    message_id: &str,
) -> Result<CommandResult<Conversation>> {
    let mut db = load_db()?;
    let Some(conversation) = db
        .conversations
        .iter_mut()
        .find(|conversation| conversation.id == conversation_id)
    else {
        return Ok(conversation_not_found(
            "mom_llama.message_branch_select",
            conversation_id,
        ));
    };
    if !conversation
        .messages
        .iter()
        .any(|message| message.id == message_id)
    {
        return Ok(message_not_found(
            "mom_llama.message_branch_select",
            message_id,
        ));
    }
    let leaf = preferred_leaf_from(conversation, message_id);
    conversation.active_leaf_message_id = Some(leaf);
    conversation.updated_at = now_ms().to_string();
    let projected = project_conversation(conversation);
    let path = save_db(&db)?;
    Ok(CommandResult::passed(
        "mom_llama.message_branch_select",
        "contracted",
        projected,
        vec![path.display().to_string()],
        Vec::new(),
        false,
        false,
    ))
}

pub fn conversation_siblings(
    conversation_id: &str,
) -> Result<CommandResult<Vec<ConversationBranchSibling>>> {
    let db = load_db()?;
    let Some(selected) = db
        .conversations
        .iter()
        .find(|conversation| conversation.id == conversation_id)
    else {
        return Ok(conversation_not_found(
            "mom_llama.conversation_siblings",
            conversation_id,
        ));
    };
    let root_conversation_id = selected
        .source_conversation_id
        .as_deref()
        .unwrap_or(&selected.id)
        .to_string();
    let source_message_id = selected.source_message_id.clone();
    let siblings = db
        .conversations
        .iter()
        .filter(|conversation| {
            if conversation.id == root_conversation_id {
                return true;
            }
            conversation.source_conversation_id.as_deref() == Some(&root_conversation_id)
                && (source_message_id.is_none()
                    || conversation.source_message_id.as_ref() == source_message_id.as_ref())
        })
        .map(|conversation| ConversationBranchSibling {
            conversation_id: conversation.id.clone(),
            title: conversation.title.clone(),
            message_count: conversation.messages.len(),
            updated_at: conversation.updated_at.clone(),
            selected: conversation.id == selected.id,
            source_conversation_id: conversation.source_conversation_id.clone(),
            source_message_id: conversation.source_message_id.clone(),
        })
        .collect::<Vec<_>>();
    Ok(CommandResult::passed(
        "mom_llama.conversation_siblings",
        "contracted",
        siblings,
        Vec::new(),
        Vec::new(),
        false,
        false,
    ))
}

pub fn draft_get(conversation_id: Option<&str>) -> Result<CommandResult<DraftMessage>> {
    let db = load_drafts()?;
    let key = draft_key(conversation_id);
    let draft = db
        .drafts
        .into_iter()
        .find(|draft| draft_key(draft.conversation_id.as_deref()) == key)
        .unwrap_or_else(|| DraftMessage {
            conversation_id: conversation_id.map(str::to_string),
            message: String::new(),
            attachment_ids: Vec::new(),
            updated_at: now_ms().to_string(),
        });
    Ok(CommandResult::passed(
        "mom_llama.draft_get",
        "contracted",
        draft,
        Vec::new(),
        Vec::new(),
        false,
        false,
    ))
}

pub fn draft_update(
    conversation_id: Option<&str>,
    message: String,
    attachment_ids: Vec<String>,
) -> Result<CommandResult<DraftMessage>> {
    let mut db = load_drafts()?;
    let key = draft_key(conversation_id);
    let draft = DraftMessage {
        conversation_id: conversation_id.map(str::to_string),
        message,
        attachment_ids,
        updated_at: now_ms().to_string(),
    };
    if draft.message.trim().is_empty() && draft.attachment_ids.is_empty() {
        db.drafts
            .retain(|existing| draft_key(existing.conversation_id.as_deref()) != key);
    } else if let Some(existing) = db
        .drafts
        .iter_mut()
        .find(|existing| draft_key(existing.conversation_id.as_deref()) == key)
    {
        *existing = draft.clone();
    } else {
        db.drafts.push(draft.clone());
    }
    let path = save_drafts(&db)?;
    Ok(CommandResult::passed(
        "mom_llama.draft_update",
        "contracted",
        draft,
        vec![path.display().to_string()],
        Vec::new(),
        false,
        false,
    ))
}

pub fn draft_clear(conversation_id: Option<&str>) -> Result<CommandResult<ConversationMutation>> {
    let mut db = load_drafts()?;
    let key = draft_key(conversation_id);
    let before = db.drafts.len();
    db.drafts
        .retain(|draft| draft_key(draft.conversation_id.as_deref()) != key);
    let path = save_drafts(&db)?;
    Ok(CommandResult::passed(
        "mom_llama.draft_clear",
        "contracted",
        ConversationMutation {
            conversation_id: conversation_id.unwrap_or(NEW_CHAT_DRAFT_KEY).to_string(),
            changed: before != db.drafts.len(),
        },
        vec![path.display().to_string()],
        Vec::new(),
        false,
        false,
    ))
}

pub fn text_attachment_import(
    conversation_id: &str,
    path: &Path,
) -> Result<CommandResult<TextAttachmentImport>> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => {
            return Ok(CommandResult::blocked(
                "mom_llama.attachment_import_text",
                "stub_blocked",
                Blocker::new(
                    "attachment_not_found",
                    format!("Text attachment {} was not found.", path.display()),
                    vec!["Choose an existing local text file.".to_string()],
                ),
            ));
        }
    };
    if !metadata.is_file() {
        return Ok(CommandResult::blocked(
            "mom_llama.attachment_import_text",
            "stub_blocked",
            Blocker::new(
                "attachment_not_file",
                format!("Text attachment {} is not a file.", path.display()),
                vec!["Choose a local text file.".to_string()],
            ),
        ));
    }
    if metadata.len() > MAX_TEXT_ATTACHMENT_BYTES {
        return Ok(CommandResult::blocked(
            "mom_llama.attachment_import_text",
            "stub_blocked",
            Blocker::new(
                "attachment_too_large",
                format!(
                    "Text attachment is {} bytes; P0 limit is {} bytes.",
                    metadata.len(),
                    MAX_TEXT_ATTACHMENT_BYTES
                ),
                vec!["Use a smaller text excerpt.".to_string()],
            ),
        ));
    }
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_lowercase();
    if !matches!(extension.as_str(), "txt" | "md" | "csv" | "json") {
        return Ok(CommandResult::blocked(
            "mom_llama.attachment_import_text",
            "stub_blocked",
            Blocker::new(
                "attachment_type_unsupported",
                "P0 only imports .txt, .md, .csv, and .json text attachments.",
                vec!["Convert the file to a plain text format.".to_string()],
            ),
        ));
    }
    let text = fs::read_to_string(path)?;
    let mut db = load_db()?;
    let Some(conversation) = db
        .conversations
        .iter_mut()
        .find(|conversation| conversation.id == conversation_id)
    else {
        return Ok(conversation_not_found(
            "mom_llama.attachment_import_text",
            conversation_id,
        ));
    };
    let now = now_ms().to_string();
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("attachment.txt")
        .to_string();
    let message = Message {
        id: Uuid::new_v4().to_string(),
        conversation_id: conversation_id.to_string(),
        role: MessageRole::User,
        content: format!("Attached text file `{file_name}`:\n\n```text\n{text}\n```"),
        created_at: now.clone(),
        parent_id: active_leaf_id(conversation),
        model: None,
        receipt_id: None,
        prompt_tokens: Some(text.split_whitespace().count()),
        completion_tokens: None,
        reasoning_content: None,
        reasoning_incomplete: false,
        branch_index: None,
        branch_count: None,
        attribution: None,
    };
    let result = TextAttachmentImport {
        conversation_id: conversation_id.to_string(),
        message_id: message.id.clone(),
        file_name,
        bytes: metadata.len(),
    };
    conversation.active_leaf_message_id = Some(message.id.clone());
    conversation.messages.push(message);
    conversation.updated_at = now;
    let path = save_db(&db)?;
    Ok(CommandResult::passed(
        "mom_llama.attachment_import_text",
        "contracted",
        result,
        vec![path.display().to_string()],
        Vec::new(),
        false,
        false,
    ))
}

pub fn active_path_messages(conversation: &Conversation) -> Vec<Message> {
    let by_id = conversation
        .messages
        .iter()
        .map(|message| (message.id.as_str(), message))
        .collect::<HashMap<_, _>>();
    let mut current = active_leaf_id(conversation);
    let mut seen = HashSet::new();
    let mut path = Vec::new();
    while let Some(message_id) = current {
        if !seen.insert(message_id.clone()) {
            break;
        }
        let Some(message) = by_id.get(message_id.as_str()) else {
            break;
        };
        path.push((*message).clone());
        current = message.parent_id.clone();
    }
    path.reverse();
    for message in &mut path {
        let mut siblings = conversation
            .messages
            .iter()
            .filter(|candidate| {
                candidate.parent_id == message.parent_id && candidate.role == message.role
            })
            .collect::<Vec<_>>();
        siblings.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        message.branch_count = Some(siblings.len());
        message.branch_index = siblings
            .iter()
            .position(|candidate| candidate.id == message.id)
            .map(|index| index + 1);
    }
    path
}

pub fn project_conversation(conversation: &Conversation) -> Conversation {
    let mut projected = conversation.clone();
    projected.messages = active_path_messages(conversation);
    projected.active_leaf_message_id = projected.messages.last().map(|message| message.id.clone());
    projected
}

pub(crate) fn strip_reserved_attribution_prefix(value: &str) -> String {
    split_reserved_attribution_prefix(value)
        .map_or_else(|| value.to_string(), |(_, content)| content.to_string())
}

fn split_reserved_attribution_prefix(value: &str) -> Option<(&str, &str)> {
    const PREFIX: &str = "Response from @";
    let trimmed = value.trim_start();
    let candidate = trimmed.get(..PREFIX.len())?;
    if !candidate.eq_ignore_ascii_case(PREFIX) {
        return None;
    }
    let remainder = &trimmed[PREFIX.len()..];
    let separator = remainder.find(':')?;
    let handle = &remainder[..separator];
    if handle.len() < 2
        || handle.len() > 48
        || !handle
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
    {
        return None;
    }
    Some((handle, remainder[separator + 1..].trim_start()))
}

fn repair_inline_attribution_prefixes(db: &mut ConversationDb) -> bool {
    let mut changed = false;
    for conversation in &mut db.conversations {
        let lineage = conversation
            .messages
            .iter()
            .map(|message| {
                (
                    message.id.clone(),
                    (
                        message.parent_id.clone(),
                        message
                            .attribution
                            .as_ref()
                            .map(|attribution| attribution.handle.clone()),
                    ),
                )
            })
            .collect::<HashMap<_, _>>();
        for message in &mut conversation.messages {
            if message.role != MessageRole::Assistant {
                continue;
            }
            let Some((handle, content)) = split_reserved_attribution_prefix(&message.content)
            else {
                continue;
            };
            let handle = handle.to_string();
            let content = content.to_string();
            let matches_own_attribution = message
                .attribution
                .as_ref()
                .is_some_and(|attribution| attribution.handle.eq_ignore_ascii_case(&handle));
            let matches_attributed_ancestor = message.attribution.is_none()
                && attributed_ancestor_matches(message.parent_id.as_deref(), &handle, &lineage);
            if matches_own_attribution || matches_attributed_ancestor {
                message.content = content;
                changed = true;
            }
        }
    }
    changed
}

fn attributed_ancestor_matches(
    parent_id: Option<&str>,
    handle: &str,
    lineage: &HashMap<String, (Option<String>, Option<String>)>,
) -> bool {
    let mut current = parent_id.map(str::to_string);
    let mut seen = HashSet::new();
    while let Some(message_id) = current {
        if !seen.insert(message_id.clone()) {
            return false;
        }
        let Some((parent, attribution)) = lineage.get(&message_id) else {
            return false;
        };
        if attribution
            .as_deref()
            .is_some_and(|candidate| candidate.eq_ignore_ascii_case(handle))
        {
            return true;
        }
        current = parent.clone();
    }
    false
}

pub fn active_leaf_id(conversation: &Conversation) -> Option<String> {
    conversation
        .active_leaf_message_id
        .as_ref()
        .filter(|id| {
            conversation
                .messages
                .iter()
                .any(|message| &message.id == *id)
        })
        .cloned()
        .or_else(|| {
            conversation
                .messages
                .last()
                .map(|message| message.id.clone())
        })
}

fn preferred_leaf_from(conversation: &Conversation, message_id: &str) -> String {
    let mut current = message_id.to_string();
    loop {
        let next = conversation
            .messages
            .iter()
            .filter(|message| message.parent_id.as_deref() == Some(current.as_str()))
            .max_by(|left, right| {
                left.created_at
                    .cmp(&right.created_at)
                    .then_with(|| left.id.cmp(&right.id))
            })
            .map(|message| message.id.clone());
        match next {
            Some(next) => current = next,
            None => return current,
        }
    }
}

fn descendant_ids(conversation: &Conversation, message_id: &str) -> HashSet<String> {
    let mut removed = HashSet::from([message_id.to_string()]);
    loop {
        let before = removed.len();
        for message in &conversation.messages {
            if message
                .parent_id
                .as_ref()
                .is_some_and(|parent| removed.contains(parent))
            {
                removed.insert(message.id.clone());
            }
        }
        if removed.len() == before {
            return removed;
        }
    }
}

pub fn load_db() -> Result<ConversationDb> {
    let settings = resolve_settings()?;
    let store = RuntimeStore::open(&settings.data_dir)?;
    let legacy_path = settings.data_dir.join(CONVERSATIONS_FILE);
    store.import_json_once::<ConversationDb>(CONVERSATIONS_NAMESPACE, &legacy_path)?;
    let mut db = store.get(CONVERSATIONS_NAMESPACE)?.unwrap_or_default();
    if repair_inline_attribution_prefixes(&mut db) {
        store.put(CONVERSATIONS_NAMESPACE, &db)?;
    }
    Ok(db)
}

pub fn save_db(db: &ConversationDb) -> Result<PathBuf> {
    let settings = resolve_settings()?;
    let store = RuntimeStore::open(&settings.data_dir)?;
    store.put(CONVERSATIONS_NAMESPACE, db)?;
    Ok(store.path().to_path_buf())
}

pub fn get_or_create_conversation(id: &str) -> Result<(ConversationDb, Conversation)> {
    let mut db = load_db()?;
    if let Some(conversation) = db
        .conversations
        .iter()
        .find(|conversation| conversation.id == id)
        .cloned()
    {
        return Ok((db, conversation));
    }
    let now = now_ms().to_string();
    let settings = resolve_settings()?;
    let conversation = Conversation {
        id: id.to_string(),
        title: if id == "default" {
            "Default chat".to_string()
        } else {
            id.to_string()
        },
        created_at: now.clone(),
        updated_at: now,
        kind: ConversationKind::Chat,
        execution_profile: ConversationExecutionProfile::default(),
        selected_model_path: settings.model_path,
        source_conversation_id: None,
        source_message_id: None,
        branch_root_message_id: None,
        active_leaf_message_id: None,
        current_skill_ids: Vec::new(),
        messages: Vec::new(),
    };
    db.conversations.insert(0, conversation.clone());
    Ok((db, conversation))
}

pub fn upsert_conversation(db: ConversationDb, conversation: Conversation) -> Result<PathBuf> {
    let settings = resolve_settings()?;
    let store = RuntimeStore::open(&settings.data_dir)?;
    store.import_json_once::<ConversationDb>(
        CONVERSATIONS_NAMESPACE,
        &settings.data_dir.join(CONVERSATIONS_FILE),
    )?;
    store.mutate(
        CONVERSATIONS_NAMESPACE,
        || db,
        |current: &mut ConversationDb| {
            if let Some(existing) = current
                .conversations
                .iter_mut()
                .find(|candidate| candidate.id == conversation.id)
            {
                for message in &conversation.messages {
                    if !existing
                        .messages
                        .iter()
                        .any(|candidate| candidate.id == message.id)
                    {
                        existing.messages.push(message.clone());
                    }
                }
                existing.updated_at = conversation.updated_at.clone();
                existing.kind = conversation.kind;
                existing.execution_profile = conversation.execution_profile.clone();
                existing.selected_model_path = conversation.selected_model_path.clone();
                existing.active_leaf_message_id = conversation.active_leaf_message_id.clone();
                if existing.title == "New chat"
                    || existing.title == "Default chat"
                    || existing.title == existing.id
                {
                    existing.title = conversation.title.clone();
                }
            } else {
                current.conversations.insert(0, conversation.clone());
            }
            current.selected_conversation_id = Some(conversation.id.clone());
            Ok(())
        },
    )?;
    Ok(store.path().to_path_buf())
}

fn conversation_not_found<T>(command: &str, id: &str) -> CommandResult<T>
where
    T: Serialize,
{
    CommandResult::blocked(
        command,
        "stub_blocked",
        Blocker::new(
            "conversation_not_found",
            format!("Conversation {id} was not found."),
            vec!["Run `mom-llama conversation list --json`.".to_string()],
        ),
    )
}

fn message_not_found<T>(command: &str, id: &str) -> CommandResult<T>
where
    T: Serialize,
{
    CommandResult::blocked(
        command,
        "stub_blocked",
        Blocker::new(
            "message_not_found",
            format!("Message {id} was not found."),
            vec!["Refresh the conversation and try again.".to_string()],
        ),
    )
}

fn snippet(content: &str, query: &str) -> String {
    let collapsed = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if query.is_empty() {
        return collapsed.chars().take(120).collect();
    }
    let lower = collapsed.to_lowercase();
    let start = lower
        .find(query)
        .map(|index| index.saturating_sub(40))
        .unwrap_or_default();
    collapsed.chars().skip(start).take(120).collect()
}

fn load_drafts() -> Result<DraftDb> {
    let settings = resolve_settings()?;
    let store = RuntimeStore::open(&settings.data_dir)?;
    store.import_json_once::<DraftDb>(DRAFTS_NAMESPACE, &settings.data_dir.join(DRAFTS_FILE))?;
    Ok(store.get(DRAFTS_NAMESPACE)?.unwrap_or_default())
}

fn save_drafts(db: &DraftDb) -> Result<PathBuf> {
    let settings = resolve_settings()?;
    let store = RuntimeStore::open(&settings.data_dir)?;
    store.put(DRAFTS_NAMESPACE, db)?;
    Ok(store.path().to_path_buf())
}

fn draft_key(conversation_id: Option<&str>) -> String {
    conversation_id.unwrap_or(NEW_CHAT_DRAFT_KEY).to_string()
}

#[cfg(test)]
mod tests {
    use super::{
        Conversation, ConversationDb, ConversationExecutionProfile, ConversationKind, Message,
        MessageAttribution, MessageRole, MessageSpeakerKind, repair_inline_attribution_prefixes,
        strip_reserved_attribution_prefix,
    };

    fn message(id: &str, parent_id: Option<&str>, role: MessageRole, content: &str) -> Message {
        Message {
            id: id.to_string(),
            conversation_id: "host".to_string(),
            role,
            content: content.to_string(),
            created_at: id.to_string(),
            parent_id: parent_id.map(str::to_string),
            model: None,
            receipt_id: None,
            prompt_tokens: None,
            completion_tokens: None,
            reasoning_content: None,
            reasoning_incomplete: false,
            branch_index: None,
            branch_count: None,
            attribution: None,
        }
    }

    #[test]
    fn reserved_attribution_prefix_is_not_assistant_content() {
        assert_eq!(
            strip_reserved_attribution_prefix(
                "Response from @default-chat: The answer belongs to the host transcript."
            ),
            "The answer belongs to the host transcript."
        );
        assert_eq!(
            strip_reserved_attribution_prefix("A normal response from @default-chat: remains."),
            "A normal response from @default-chat: remains."
        );
    }

    #[test]
    fn legacy_copied_prefix_is_repaired_only_when_structural_attribution_supports_it() {
        let mut attributed = message("a", None, MessageRole::Assistant, "First answer");
        attributed.attribution = Some(MessageAttribution {
            kind: MessageSpeakerKind::LiveChat,
            source_id: "source".to_string(),
            handle: "default-chat".to_string(),
            label: "Default chat".to_string(),
            version: 1,
            invocation_id: "invocation".to_string(),
            target_order: 0,
        });
        let user = message("u", Some("a"), MessageRole::User, "Follow up");
        let copied = message(
            "b",
            Some("u"),
            MessageRole::Assistant,
            "Response from @default-chat: A direct host answer",
        );
        let unrelated = message(
            "c",
            None,
            MessageRole::Assistant,
            "Response from @unrelated-chat: Preserve this unverified literal",
        );
        let mut db = ConversationDb {
            conversations: vec![Conversation {
                id: "host".to_string(),
                title: "Host".to_string(),
                created_at: "1".to_string(),
                updated_at: "4".to_string(),
                kind: ConversationKind::Chat,
                execution_profile: ConversationExecutionProfile::default(),
                selected_model_path: None,
                source_conversation_id: None,
                source_message_id: None,
                branch_root_message_id: None,
                active_leaf_message_id: Some("b".to_string()),
                current_skill_ids: Vec::new(),
                messages: vec![attributed, user, copied, unrelated],
            }],
            selected_conversation_id: Some("host".to_string()),
        };

        assert!(repair_inline_attribution_prefixes(&mut db));
        assert_eq!(
            db.conversations[0].messages[2].content,
            "A direct host answer"
        );
        assert_eq!(
            db.conversations[0].messages[3].content,
            "Response from @unrelated-chat: Preserve this unverified literal"
        );
        assert!(!repair_inline_attribution_prefixes(&mut db));
    }
}
