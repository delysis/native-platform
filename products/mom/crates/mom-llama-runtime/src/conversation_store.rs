use crate::config::resolve_settings;
use crate::now_ms;
use crate::receipts::{Blocker, CommandResult};
use crate::store::RuntimeStore;
use anyhow::Result;
use serde::{Deserialize, Serialize};
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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Conversation {
    pub id: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    pub selected_model_path: Option<PathBuf>,
    #[serde(default)]
    pub source_conversation_id: Option<String>,
    #[serde(default)]
    pub source_message_id: Option<String>,
    #[serde(default)]
    pub branch_root_message_id: Option<String>,
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
    let conversation = Conversation {
        id: Uuid::new_v4().to_string(),
        title: title.unwrap_or_else(|| "New chat".to_string()),
        created_at: now.clone(),
        updated_at: now,
        selected_model_path: settings.model_path,
        source_conversation_id: None,
        source_message_id: None,
        branch_root_message_id: None,
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
        conversation,
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
        .iter_mut()
        .find(|message| message.id == message_id)
    else {
        return Ok(message_not_found("mom_llama.message_edit", message_id));
    };
    message.content = content;
    conversation.updated_at = now_ms().to_string();
    let result = message.clone();
    let path = save_db(&db)?;
    Ok(CommandResult::passed(
        "mom_llama.message_edit",
        "contracted",
        result,
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
    let before = conversation.messages.len();
    conversation
        .messages
        .retain(|message| message.id != message_id);
    let changed = before != conversation.messages.len();
    if !changed {
        return Ok(message_not_found("mom_llama.message_delete", message_id));
    }
    conversation.updated_at = now_ms().to_string();
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
        selected_model_path: source.selected_model_path,
        source_conversation_id: Some(source.id.clone()),
        source_message_id: Some(message_id.to_string()),
        branch_root_message_id: Some(message_id.to_string()),
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
        parent_id: conversation
            .messages
            .last()
            .map(|message| message.id.clone()),
        model: None,
        receipt_id: None,
        prompt_tokens: Some(text.split_whitespace().count()),
        completion_tokens: None,
    };
    let result = TextAttachmentImport {
        conversation_id: conversation_id.to_string(),
        message_id: message.id.clone(),
        file_name,
        bytes: metadata.len(),
    };
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

pub fn load_db() -> Result<ConversationDb> {
    let settings = resolve_settings()?;
    let store = RuntimeStore::open(&settings.data_dir)?;
    let legacy_path = settings.data_dir.join(CONVERSATIONS_FILE);
    store.import_json_once::<ConversationDb>(CONVERSATIONS_NAMESPACE, &legacy_path)?;
    Ok(store.get(CONVERSATIONS_NAMESPACE)?.unwrap_or_default())
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
        selected_model_path: settings.model_path,
        source_conversation_id: None,
        source_message_id: None,
        branch_root_message_id: None,
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
                existing.selected_model_path = conversation.selected_model_path.clone();
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
