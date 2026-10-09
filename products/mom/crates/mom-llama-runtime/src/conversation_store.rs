use crate::config::resolve_settings;
use crate::now_ms;
use crate::receipts::{Blocker, CommandResult};
use crate::store::RuntimeStore;
use anyhow::Result;
use llama_native_types::SamplingConfig;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub(crate) const CONVERSATIONS_NAMESPACE: &str = "conversations.v2";
pub(crate) const DRAFTS_NAMESPACE: &str = "drafts.v2";
const NEW_CHAT_DRAFT_KEY: &str = "__new_chat__";

pub(crate) fn valid_occurrence_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control)
}

// One role type, with the same serde wire names and public module path.
pub use workspace_document::MessageRole;

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
    pub attachment_ids: Vec<String>,
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
    pub recipient_ids: Vec<String>,
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
    pub kind: ConversationKind,
    pub active_leaf_message_id: Option<String>,
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
    #[serde(default)]
    pub attachment_id: String,
    pub message_id: String,
    pub file_name: String,
    pub bytes: u64,
}

pub(crate) const NEW_CHAT_CONTEXT_NAMESPACE: &str = "conversation.new-draft-context.v1";

pub(crate) const NEW_CHAT_RECIPIENTS_NAMESPACE: &str = "conversation.new-draft-recipients.v1";

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DraftRecipients {
    pub recipient_ids: Vec<String>,
    pub name: Option<String>,
}

pub fn conversation_draft_recipients() -> Result<DraftRecipients> {
    let store = RuntimeStore::current()?;
    if let Some(recipients) = store.get(NEW_CHAT_RECIPIENTS_NAMESPACE)? {
        return Ok(recipients);
    }
    let persona: Option<String> = store.get(NEW_CHAT_CONTEXT_NAMESPACE)?.unwrap_or_default();
    Ok(DraftRecipients {
        recipient_ids: persona.into_iter().collect(),
        name: None,
    })
}

/// Recipients are stable identities, never inferred from text in the composer.
/// This changes the single unsent draft without creating a chat or contact group.
pub fn conversation_draft_recipients_update(
    recipient_ids: Vec<String>,
    name: Option<String>,
) -> Result<CommandResult<DraftRecipients>> {
    crate::personas::ensure_builtin_catalog()?;
    let store = RuntimeStore::current()?;
    let result = store.mutate_documents(
        CONVERSATIONS_NAMESPACE,
        ConversationDb::default,
        |db, documents| {
            let rejected = |message: &str| {
                Ok(Err(Blocker::new(
                    "invalid_draft_recipients",
                    message,
                    Vec::new(),
                )))
            };
            if db
                .selected_conversation_id
                .as_deref()
                .is_some_and(|id| id != "default")
            {
                return rejected("The draft is no longer selected");
            }
            if recipient_ids.len() > 4 {
                return rejected("At most four contacts may be selected");
            }
            let mut unique = HashSet::new();
            for id in &recipient_ids {
                if !unique.insert(id) {
                    return rejected("Duplicate contact");
                }
                if !db.conversations.iter().any(|contact| {
                    contact.id == *id && contact.kind == ConversationKind::PersonaTemplate
                }) {
                    return rejected("Contact is unavailable");
                }
            }
            let name = name
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty());
            if name.as_ref().is_some_and(|value| {
                value.chars().count() > 120 || value.chars().any(char::is_control)
            }) {
                return rejected("Invalid group name");
            }
            let recipients = DraftRecipients {
                recipient_ids,
                name,
            };
            let persona =
                (recipients.recipient_ids.len() == 1).then(|| recipients.recipient_ids[0].clone());
            documents.put_bytes(NEW_CHAT_CONTEXT_NAMESPACE, &serde_json::to_vec(&persona)?)?;
            documents.put_bytes(
                NEW_CHAT_RECIPIENTS_NAMESPACE,
                &serde_json::to_vec(&recipients)?,
            )?;
            db.selected_conversation_id = Some("default".to_string());
            Ok(Ok(recipients))
        },
    )?;
    Ok(match result {
        Ok(recipients) => CommandResult::passed(
            "mom_llama.conversation_draft_recipients_update",
            "contracted",
            recipients,
            vec![store.path().display().to_string()],
            Vec::new(),
            false,
            false,
        ),
        Err(blocker) => CommandResult::blocked(
            "mom_llama.conversation_draft_recipients_update",
            "blocked_recipients",
            blocker,
        ),
    })
}

/// Opening a composer selects one durable draft, never a saved conversation.
pub fn conversation_draft_open(persona: Option<String>) -> Result<CommandResult<Option<String>>> {
    let store = RuntimeStore::current()?;
    let result = store.mutate_documents(
        CONVERSATIONS_NAMESPACE,
        ConversationDb::default,
        |db, documents| {
            let current: Option<String> = documents
                .get(NEW_CHAT_CONTEXT_NAMESPACE)?
                .unwrap_or_default();
            let explicit_persona = persona.is_some();
            let selected = persona.or(current);
            if explicit_persona {
                let recipients = DraftRecipients {
                    recipient_ids: selected.iter().cloned().collect(),
                    name: None,
                };
                documents.put_bytes(
                    NEW_CHAT_RECIPIENTS_NAMESPACE,
                    &serde_json::to_vec(&recipients)?,
                )?;
            }
            if let Some(id) = &selected {
                anyhow::ensure!(
                    db.conversations
                        .iter()
                        .any(|conversation| conversation.id == *id
                            && conversation.kind == ConversationKind::PersonaTemplate),
                    "Persona is unavailable"
                );
            }
            documents.put_bytes(NEW_CHAT_CONTEXT_NAMESPACE, &serde_json::to_vec(&selected)?)?;
            db.selected_conversation_id = Some("default".to_string());
            Ok(selected)
        },
    )?;
    Ok(CommandResult::passed(
        "mom_llama.conversation_draft_open",
        "contracted",
        result,
        Vec::new(),
        Vec::new(),
        false,
        false,
    ))
}

pub fn conversation_draft_preview() -> Result<Option<Conversation>> {
    let persona: Option<String> = RuntimeStore::current()?
        .get(NEW_CHAT_CONTEXT_NAMESPACE)?
        .unwrap_or_default();
    let Some(persona) = persona else {
        return Ok(None);
    };
    let mut preview = load_db()?
        .conversations
        .into_iter()
        .find(|conversation| {
            conversation.id == persona && conversation.kind == ConversationKind::PersonaTemplate
        })
        .ok_or_else(|| anyhow::anyhow!("Draft Persona is unavailable"))?;
    preview.source_conversation_id = Some(preview.id.clone());
    preview.id = "default".to_string();
    preview.messages.clear();
    preview.active_leaf_message_id = None;
    Ok(Some(preview))
}

/// Materialize only after the shared draft has a submitted message or attachment.
pub fn conversation_draft_submit() -> Result<CommandResult<Conversation>> {
    crate::personas::ensure_builtin_catalog()?;
    let store = RuntimeStore::current()?;
    let _attachments = crate::attachments::lock_attachment_lifecycle()?;
    let settings = resolve_settings()?;
    let result = store.mutate_documents(
        CONVERSATIONS_NAMESPACE,
        ConversationDb::default,
        |db, documents| {
            let mut drafts: DraftDb = documents.get(DRAFTS_NAMESPACE)?.unwrap_or_default();
            let Some(index) = drafts.drafts.iter().position(|draft| {
                draft_key(draft.conversation_id.as_deref()) == NEW_CHAT_DRAFT_KEY
            }) else {
                return Ok(Err(Blocker::new("empty_draft", "Empty draft", Vec::new())));
            };
            if drafts.drafts[index].message.trim().is_empty()
                && drafts.drafts[index].attachment_ids.is_empty()
            {
                return Ok(Err(Blocker::new("empty_draft", "Empty draft", Vec::new())));
            }
            let persona: Option<String> = documents
                .get(NEW_CHAT_CONTEXT_NAMESPACE)?
                .unwrap_or_default();
            let id = Uuid::new_v4().to_string();
            let now = now_ms().to_string();
            let recipients: DraftRecipients = documents
                .get(NEW_CHAT_RECIPIENTS_NAMESPACE)?
                .unwrap_or_else(|| DraftRecipients {
                    recipient_ids: persona.iter().cloned().collect(),
                    name: None,
                });
            for recipient in &recipients.recipient_ids {
                anyhow::ensure!(
                    db.conversations
                        .iter()
                        .any(|contact| contact.id == *recipient
                            && contact.kind == ConversationKind::PersonaTemplate),
                    "Contact is unavailable"
                );
            }
            let mut conversation = if let Some(persona) = persona {
                match crate::personas::instantiate_from_documents(
                    db, documents, &persona, None, &id, &now,
                )? {
                    Ok(conversation) => conversation,
                    Err(blocker) => return Ok(Err(blocker)),
                }
            } else {
                let conversation = new_conversation(
                    title_from_text(&drafts.drafts[index].message),
                    &settings,
                    id,
                    now,
                );
                db.selected_conversation_id = Some(conversation.id.clone());
                db.conversations.insert(0, conversation.clone());
                conversation
            };
            conversation.recipient_ids = recipients.recipient_ids;
            if let Some(name) = recipients.name {
                conversation.title = name;
            }
            let saved = db
                .conversations
                .iter_mut()
                .find(|saved| saved.id == conversation.id)
                .expect("newly created conversation");
            *saved = conversation.clone();
            documents.put_bytes(
                NEW_CHAT_RECIPIENTS_NAMESPACE,
                &serde_json::to_vec(&DraftRecipients::default())?,
            )?;
            // Creation and transfer share one encrypted store transaction. A repeated
            // submission sees no new draft and cannot create another empty chat.
            crate::attachments::transfer_new_draft_attachments(
                documents,
                &drafts.drafts[index].attachment_ids,
                &conversation.id,
            )?;
            drafts.drafts[index].conversation_id = Some(conversation.id.clone());
            documents.put_bytes(DRAFTS_NAMESPACE, &serde_json::to_vec(&drafts)?)?;
            documents.put_bytes(
                NEW_CHAT_CONTEXT_NAMESPACE,
                &serde_json::to_vec(&None::<String>)?,
            )?;
            Ok(Ok(conversation))
        },
    )?;
    match result {
        Ok(conversation) => Ok(CommandResult::passed(
            "mom_llama.conversation_draft_submit",
            "contracted",
            conversation,
            vec![store.path().display().to_string()],
            Vec::new(),
            false,
            false,
        )),
        Err(blocker) => Ok(CommandResult::blocked(
            "mom_llama.conversation_draft_submit",
            "blocked_draft",
            blocker,
        )),
    }
}

pub fn conversation_new(title: Option<String>) -> Result<CommandResult<Conversation>> {
    let mut db = load_db()?;
    let now = now_ms().to_string();
    let settings = resolve_settings()?;
    let id = Uuid::new_v4().to_string();
    let conversation = new_conversation(title, &settings, id, now);
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

fn new_conversation(
    title: Option<String>,
    settings: &crate::config::Settings,
    id: String,
    now: String,
) -> Conversation {
    let title = title.unwrap_or_else(|| "New chat".to_string());
    let mention_handle = new_chat_handle(&title, &id);
    Conversation {
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
        selected_model_path: settings.model_path.clone(),
        source_conversation_id: None,
        source_message_id: None,
        branch_root_message_id: None,
        active_leaf_message_id: None,
        recipient_ids: Vec::new(),
        current_skill_ids: Vec::new(),
        messages: Vec::new(),
    }
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
        db.conversations
            .into_iter()
            .map(|mut conversation| {
                conversation.title = display_title(&conversation);
                conversation
            })
            .collect::<Vec<_>>(),
        Vec::new(),
        Vec::new(),
        false,
        false,
    ))
}

pub fn conversation_select(id: &str) -> Result<CommandResult<Conversation>> {
    let store = RuntimeStore::current()?;
    let Some(conversation) = select_conversation_in_store(&store, id)? else {
        return Ok(conversation_not_found("mom_llama.conversation_select", id));
    };
    Ok(CommandResult::passed(
        "mom_llama.conversation_select",
        "contracted",
        project_conversation(&conversation),
        vec![store.path().display().to_string()],
        Vec::new(),
        false,
        false,
    ))
}

fn select_conversation_in_store(store: &RuntimeStore, id: &str) -> Result<Option<Conversation>> {
    store.mutate_documents(
        CONVERSATIONS_NAMESPACE,
        ConversationDb::default,
        |db, documents| {
            crate::personas::reject_removed_conversation_writes_from_documents(db, documents)?;
            let Some(conversation) = db.conversations.iter().find(|item| item.id == id).cloned()
            else {
                return Ok(None);
            };
            db.selected_conversation_id = Some(id.to_string());
            Ok(Some(conversation))
        },
    )
}

pub fn conversation_search(query: &str) -> Result<CommandResult<Vec<ConversationSearchHit>>> {
    let db = load_db()?;
    let query = query.trim().to_lowercase();
    let hits = db
        .conversations
        .iter()
        .filter_map(|conversation| {
            let title = display_title(conversation);
            let title_matches = query.is_empty() || title.to_lowercase().contains(&query);
            let message_match = conversation.messages.iter().find(|message| {
                query.is_empty() || message.content.to_lowercase().contains(&query)
            });
            if !title_matches && message_match.is_none() {
                return None;
            }
            let snippet = message_match
                .map(|message| snippet(&message.content, &query))
                .unwrap_or_else(|| title.clone());
            Some(ConversationSearchHit {
                conversation_id: conversation.id.clone(),
                kind: conversation.kind,
                active_leaf_message_id: conversation.active_leaf_message_id.clone(),
                title,
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
    let store = RuntimeStore::current()?;
    let Some(result) = rename_conversation_in_store(&store, id, &title)? else {
        return Ok(conversation_not_found("mom_llama.conversation_rename", id));
    };
    Ok(CommandResult::passed(
        "mom_llama.conversation_rename",
        "contracted",
        result,
        vec![store.path().display().to_string()],
        Vec::new(),
        false,
        false,
    ))
}

fn rename_conversation_in_store(
    store: &RuntimeStore,
    id: &str,
    title: &str,
) -> Result<Option<Conversation>> {
    // Native reply commits run concurrently with sidebar commands. Read and
    // change only this conversation's metadata under the store's IMMEDIATE
    // transaction; never replace the registry from a pre-transaction snapshot.
    store.mutate_documents(
        CONVERSATIONS_NAMESPACE,
        ConversationDb::default,
        |db, documents| {
            crate::personas::reject_removed_conversation_writes_from_documents(db, documents)?;
            let Some(conversation) = db.conversations.iter_mut().find(|item| item.id == id) else {
                return Ok(None);
            };
            let title = title.trim();
            conversation.title = if title.is_empty() {
                "Untitled conversation".to_string()
            } else {
                title.to_string()
            };
            conversation.updated_at = now_ms().to_string();
            Ok(Some(conversation.clone()))
        },
    )
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
    let expected_db = db.clone();
    let Some(index) = db
        .conversations
        .iter()
        .position(|conversation| conversation.id == id)
    else {
        return Ok(conversation_not_found("mom_llama.conversation_delete", id));
    };
    let removed = db.conversations.remove(index);
    let mut removed_attachment_ids = removed
        .messages
        .iter()
        .flat_map(|message| message.attachment_ids.iter().cloned())
        .collect::<BTreeSet<_>>();
    let mut drafts = load_drafts()?;
    let expected_drafts = drafts.clone();
    drafts.drafts.retain(|draft| {
        if draft.conversation_id.as_deref() == Some(id) {
            removed_attachment_ids.extend(draft.attachment_ids.iter().cloned());
            false
        } else {
            true
        }
    });
    if db.selected_conversation_id.as_deref() == Some(id) {
        db.selected_conversation_id = db
            .conversations
            .first()
            .map(|conversation| conversation.id.clone());
    }
    let path = crate::attachments::persist_conversations_with_attachment_gc(
        &db,
        &expected_db,
        Some(&drafts),
        Some(&expected_drafts),
        &removed_attachment_ids,
        &BTreeSet::from([id.to_string()]),
    )?;
    Ok(CommandResult::passed(
        "mom_llama.conversation_delete",
        "contracted",
        ConversationMutation {
            conversation_id: id.to_string(),
            changed: true,
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
    let handle = match crate::personas::validate_imported_conversation_handle(
        &db,
        &imported.execution_profile.mention_handle,
        &imported.title,
    )? {
        Ok(handle) => handle,
        Err(blocker) => {
            return Ok(CommandResult::blocked(
                "mom_llama.conversation_import",
                "stub_blocked",
                blocker,
            ));
        }
    };
    imported.execution_profile.mention_handle = handle;
    if imported.execution_profile.model_path.is_none() {
        imported.execution_profile.model_path = imported.selected_model_path.clone();
    }
    if imported.id.trim().is_empty()
        || db
            .conversations
            .iter()
            .any(|conversation| conversation.id == imported.id)
    {
        imported.id = Uuid::new_v4().to_string();
    }
    for message in &mut imported.messages {
        message.conversation_id = imported.id.clone();
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
    if matches!(&message.role, MessageRole::System | MessageRole::Tool) {
        return Ok(CommandResult::blocked(
            "mom_llama.message_edit",
            "stub_blocked",
            Blocker::new(
                "message_role_not_editable",
                format!("{:?} messages cannot be edited.", message.role),
                vec!["Edit the conversation system message or rerun the tool instead.".to_string()],
            ),
        ));
    }
    let attribution = message.attribution.clone();
    let mut edited = message;
    edited.id = Uuid::new_v4().to_string();
    edited.content = content;
    edited.created_at = now_ms().to_string();
    edited.receipt_id = None;
    edited.branch_index = None;
    edited.branch_count = None;
    conversation.messages.push(edited.clone());
    conversation.active_leaf_message_id = Some(
        attribution
            .as_ref()
            .and_then(|attribution| {
                clone_later_attributed_peers(conversation, message_id, &edited.id, attribution)
            })
            .unwrap_or_else(|| edited.id.clone()),
    );
    conversation.updated_at = now_ms().to_string();
    if conversation.kind == ConversationKind::PersonaTemplate {
        conversation.execution_profile.version =
            conversation.execution_profile.version.saturating_add(1);
    }
    let persona_version =
        (conversation.kind == ConversationKind::PersonaTemplate).then(|| conversation.clone());
    let path = if let Some(persona) = persona_version {
        crate::personas::save_persona_with_version(&db, &persona)?
    } else {
        save_db(&db)?
    };
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
    let expected_db = db.clone();
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
    let Some(target) = conversation
        .messages
        .iter()
        .find(|message| message.id == message_id)
        .cloned()
    else {
        return Ok(message_not_found("mom_llama.message_delete", message_id));
    };
    let parent_id = target.parent_id.clone();
    let removed = if target.attribution.is_some() {
        // Mention results are independent responses that happen to be linked as a
        // chain so the ordinary single-leaf projection can display all of them.
        // Splice a deleted result out of that display chain rather than treating
        // every later peer (and the user's subsequent conversation) as its
        // semantic descendants.
        for child in conversation
            .messages
            .iter_mut()
            .filter(|message| message.parent_id.as_deref() == Some(message_id))
        {
            child.parent_id.clone_from(&parent_id);
        }
        HashSet::from([message_id.to_string()])
    } else {
        descendant_ids(conversation, message_id)
    };
    let removed_attachment_ids = conversation
        .messages
        .iter()
        .filter(|message| removed.contains(&message.id))
        .flat_map(|message| message.attachment_ids.iter().cloned())
        .collect::<BTreeSet<_>>();
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
    let path = crate::attachments::persist_conversations_with_attachment_gc(
        &db,
        &expected_db,
        None,
        None,
        &removed_attachment_ids,
        &BTreeSet::new(),
    )?;
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
    let active = active_path_messages(&source);
    let Some(index) = active.iter().position(|message| message.id == message_id) else {
        return Ok(message_not_found("mom_llama.conversation_fork", message_id));
    };
    let now = now_ms().to_string();
    let fork_id = Uuid::new_v4().to_string();
    let mut messages = active[..=index].to_vec();
    for message in &mut messages {
        message.conversation_id = fork_id.clone();
    }
    crate::attachments::snapshot_message_attachments(&fork_id, &mut messages)?;
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
        recipient_ids: source.recipient_ids,
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
    let previous_attachment_ids = db
        .drafts
        .iter()
        .find(|draft| draft_key(draft.conversation_id.as_deref()) == key)
        .map(|draft| {
            draft
                .attachment_ids
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
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
    let retained_attachment_ids = draft
        .attachment_ids
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let removed_attachment_ids = previous_attachment_ids
        .difference(&retained_attachment_ids)
        .cloned()
        .collect::<BTreeSet<_>>();
    let path = crate::attachments::persist_drafts_with_attachment_gc(&db, &removed_attachment_ids)?;
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
    let mut removed_attachment_ids = BTreeSet::new();
    db.drafts.retain(|draft| {
        if draft_key(draft.conversation_id.as_deref()) == key {
            removed_attachment_ids.extend(draft.attachment_ids.iter().cloned());
            false
        } else {
            true
        }
    });
    let path = crate::attachments::persist_drafts_with_attachment_gc(&db, &removed_attachment_ids)?;
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
    scope: &crate::OperationScope,
    conversation_id: &str,
    path: &Path,
) -> Result<CommandResult<TextAttachmentImport>> {
    let imported = crate::attachments::attachment_import(scope, conversation_id, path)?;
    if imported.status == "blocked" {
        return Ok(CommandResult::blocked(
            "mom_llama.attachment_import_text",
            &imported.readiness,
            imported.blocker.unwrap_or_else(|| {
                Blocker::new(
                    "attachment_import_blocked",
                    "The attachment could not be staged.",
                    vec!["Choose another file.".to_string()],
                )
            }),
        ));
    }
    let attachment = imported
        .result
        .map(|output| output.attachment)
        .ok_or_else(|| anyhow::anyhow!("passed attachment import had no result"))?;
    Ok(CommandResult::passed(
        "mom_llama.attachment_import_text",
        "contracted",
        TextAttachmentImport {
            conversation_id: conversation_id.to_string(),
            attachment_id: attachment.id,
            message_id: String::new(),
            file_name: attachment.file_name,
            bytes: attachment.bytes,
        },
        imported.receipt.changed_paths,
        imported.receipt.artifacts_produced,
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

pub(crate) fn is_placeholder_title(title: &str, conversation_id: &str) -> bool {
    matches!(title, "New chat" | "Default chat" | "Untitled conversation")
        || title == conversation_id
}

fn title_from_text(text: &str) -> Option<String> {
    text.lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| {
            let line = line.trim();
            let mut title = line.chars().take(64).collect::<String>();
            if title.len() < line.len() {
                title.push_str("...");
            }
            title
        })
}

pub(crate) fn first_line_title(messages: &[Message]) -> String {
    messages
        .iter()
        .find(|message| message.role == MessageRole::User)
        .and_then(|message| title_from_text(&message.content))
        .unwrap_or_else(|| "New chat".to_string())
}

pub(crate) fn display_title(conversation: &Conversation) -> String {
    if conversation.kind == ConversationKind::Chat
        && is_placeholder_title(&conversation.title, &conversation.id)
    {
        first_line_title(&active_path_messages(conversation))
    } else {
        conversation.title.clone()
    }
}

pub fn project_conversation(conversation: &Conversation) -> Conversation {
    let mut projected = conversation.clone();
    projected.title = display_title(conversation);
    normalize_conversation_model_paths(&mut projected);
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

fn clone_later_attributed_peers(
    conversation: &mut Conversation,
    source_message_id: &str,
    edited_message_id: &str,
    attribution: &MessageAttribution,
) -> Option<String> {
    if attribution.kind == MessageSpeakerKind::Synthesis {
        return None;
    }

    let mut source_parent = source_message_id.to_string();
    let mut cloned_parent = edited_message_id.to_string();
    let mut target_order = attribution.target_order;
    let mut cloned_any = false;

    loop {
        let next = conversation
            .messages
            .iter()
            .filter(|message| {
                message.parent_id.as_deref() == Some(source_parent.as_str())
                    && message.role == MessageRole::Assistant
                    && message.attribution.as_ref().is_some_and(|candidate| {
                        candidate.invocation_id == attribution.invocation_id
                            && candidate.kind != MessageSpeakerKind::Synthesis
                            && candidate.target_order > target_order
                    })
            })
            .min_by(|left, right| {
                let left_order = left
                    .attribution
                    .as_ref()
                    .map_or(usize::MAX, |item| item.target_order);
                let right_order = right
                    .attribution
                    .as_ref()
                    .map_or(usize::MAX, |item| item.target_order);
                left_order
                    .cmp(&right_order)
                    .then_with(|| left.created_at.cmp(&right.created_at))
                    .then_with(|| left.id.cmp(&right.id))
            })
            .cloned();
        let Some(mut next) = next else {
            break;
        };
        let original_id = next.id.clone();
        target_order = next
            .attribution
            .as_ref()
            .map_or(target_order, |item| item.target_order);
        next.id = Uuid::new_v4().to_string();
        next.parent_id = Some(cloned_parent);
        next.created_at = now_ms().to_string();
        next.branch_index = None;
        next.branch_count = None;
        source_parent = original_id;
        cloned_parent = next.id.clone();
        conversation.messages.push(next);
        cloned_any = true;
    }

    cloned_any.then_some(cloned_parent)
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
    Ok(store.get(CONVERSATIONS_NAMESPACE)?.unwrap_or_default())
}

fn normalize_conversation_model_paths(conversation: &mut Conversation) -> bool {
    let previous_selected = conversation.selected_model_path.take();
    let previous_model = conversation.execution_profile.model_path.take();
    let previous_mmproj = conversation.execution_profile.mmproj_path.take();
    let selected = crate::config::normalize_optional_path(previous_selected.clone());
    let model = crate::config::normalize_optional_path(previous_model.clone());
    let mmproj = crate::config::normalize_optional_path(previous_mmproj.clone());
    let changed =
        previous_selected != selected || previous_model != model || previous_mmproj != mmproj;
    conversation.selected_model_path = selected;
    conversation.execution_profile.model_path = model;
    conversation.execution_profile.mmproj_path = mmproj;
    changed
}

pub fn save_db(db: &ConversationDb) -> Result<PathBuf> {
    let settings = resolve_settings()?;
    let store = RuntimeStore::open(&settings.data_dir)?;
    store.mutate_documents(
        CONVERSATIONS_NAMESPACE,
        ConversationDb::default,
        |stored, documents| {
            let next = db.clone();
            crate::personas::reject_removed_conversation_writes_from_documents(&next, documents)?;
            *stored = next;
            Ok(())
        },
    )?;
    Ok(store.path().to_path_buf())
}

pub fn get_or_create_conversation(id: &str) -> Result<(ConversationDb, Conversation)> {
    if !valid_occurrence_id(id) {
        anyhow::bail!("invalid conversation occurrence identity");
    }
    let imported = load_db()?;
    let initially_present = imported
        .conversations
        .iter()
        .any(|conversation| conversation.id == id);
    let settings = resolve_settings()?;
    let store = RuntimeStore::open(&settings.data_dir)?;
    store.mutate_documents(
        CONVERSATIONS_NAMESPACE,
        || imported,
        |db: &mut ConversationDb, documents| {
            crate::personas::reject_removed_conversation_id_from_documents(id, documents)?;
            crate::personas::reject_removed_conversation_writes_from_documents(db, documents)?;
            if let Some(conversation) = db
                .conversations
                .iter()
                .find(|conversation| conversation.id == id)
                .cloned()
            {
                return Ok((db.clone(), conversation));
            }
            if initially_present {
                anyhow::bail!("conversation was removed before chat admission");
            }
            let now = now_ms().to_string();
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
                selected_model_path: settings.model_path.clone(),
                source_conversation_id: None,
                source_message_id: None,
                branch_root_message_id: None,
                active_leaf_message_id: None,
                recipient_ids: Vec::new(),
                current_skill_ids: Vec::new(),
                messages: Vec::new(),
            };
            db.conversations.insert(0, conversation.clone());
            Ok((db.clone(), conversation))
        },
    )
}

pub fn upsert_conversation(db: ConversationDb, conversation: Conversation) -> Result<PathBuf> {
    let settings = resolve_settings()?;
    let store = RuntimeStore::open(&settings.data_dir)?;
    store.mutate_documents(
        CONVERSATIONS_NAMESPACE,
        || db,
        |current: &mut ConversationDb, documents| {
            crate::personas::reject_removed_conversation_id_from_documents(
                &conversation.id,
                documents,
            )?;
            crate::personas::reject_removed_conversation_writes_from_documents(current, documents)?;
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

pub(crate) fn load_drafts() -> Result<DraftDb> {
    let settings = resolve_settings()?;
    let store = RuntimeStore::open(&settings.data_dir)?;
    Ok(store.get(DRAFTS_NAMESPACE)?.unwrap_or_default())
}

fn draft_key(conversation_id: Option<&str>) -> String {
    match conversation_id {
        None | Some("default") => NEW_CHAT_DRAFT_KEY.to_string(),
        Some(id) => id.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Conversation, ConversationExecutionProfile, ConversationKind, Message, MessageRole,
        project_conversation, strip_reserved_attribution_prefix,
    };
    use std::path::PathBuf;

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
            attachment_ids: Vec::new(),
        }
    }

    #[test]
    fn placeholder_titles_follow_the_first_user_message_and_manual_titles_survive() {
        let mut conversation = Conversation {
            id: "title-test".into(),
            title: "New chat".into(),
            created_at: "1".into(),
            updated_at: "1".into(),
            kind: ConversationKind::Chat,
            execution_profile: ConversationExecutionProfile::default(),
            selected_model_path: None,
            source_conversation_id: None,
            source_message_id: None,
            branch_root_message_id: None,
            active_leaf_message_id: Some("reply".into()),
            recipient_ids: Vec::new(),
            current_skill_ids: vec![],
            messages: vec![
                message("user", None, MessageRole::User, "Actual subject\nDetails"),
                message("reply", Some("user"), MessageRole::Assistant, "Response"),
            ],
        };
        assert_eq!(super::display_title(&conversation), "Actual subject");
        assert_eq!(project_conversation(&conversation).title, "Actual subject");
        assert_eq!(
            conversation.title, "New chat",
            "projection does not rewrite stored history"
        );
        conversation.title = "Chosen name".into();
        assert_eq!(super::display_title(&conversation), "Chosen name");
        assert!(super::title_from_text(" \n ").is_none());
        assert!(
            super::title_from_text(&"é".repeat(80))
                .expect("Unicode title")
                .ends_with("...")
        );
    }

    #[test]
    fn stored_messages_require_current_attachment_linkage() {
        let mut encoded =
            serde_json::to_value(message("current", None, MessageRole::User, "source"))
                .expect("encode message");
        encoded
            .as_object_mut()
            .expect("message object")
            .remove("attachment_ids");
        assert!(serde_json::from_value::<Message>(encoded).is_err());
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
    fn projected_legacy_blank_model_paths_do_not_mask_fallbacks() {
        let mut conversation = Conversation {
            id: "legacy-blank-model".to_string(),
            title: "Legacy".to_string(),
            created_at: "1".to_string(),
            updated_at: "1".to_string(),
            kind: ConversationKind::Chat,
            execution_profile: ConversationExecutionProfile::default(),
            selected_model_path: Some(PathBuf::new()),
            source_conversation_id: None,
            source_message_id: None,
            branch_root_message_id: None,
            active_leaf_message_id: None,
            recipient_ids: Vec::new(),
            current_skill_ids: Vec::new(),
            messages: Vec::new(),
        };
        conversation.execution_profile.model_path = Some(PathBuf::from("   "));
        conversation.execution_profile.mmproj_path = Some(PathBuf::new());

        let projected = project_conversation(&conversation);
        assert_eq!(projected.selected_model_path, None);
        assert_eq!(projected.execution_profile.model_path, None);
        assert_eq!(projected.execution_profile.mmproj_path, None);
    }

    #[cfg(unix)]
    fn with_rename_store(test: impl FnOnce(&crate::store::RuntimeStore) -> anyhow::Result<()>) {
        let directory = std::env::temp_dir().join(format!("mom-rename-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).expect("create isolated store directory");
        struct Remove(PathBuf);
        impl Drop for Remove {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _remove = Remove(directory.clone());
        let store =
            crate::store::RuntimeStore::open(&directory).expect("open encrypted test store");
        test(&store).expect("rename store regression");
    }

    #[cfg(unix)]
    fn rename_fixture(id: &str) -> Conversation {
        Conversation {
            id: id.into(),
            title: "Original title".into(),
            created_at: "1".into(),
            updated_at: "1".into(),
            kind: ConversationKind::Chat,
            execution_profile: ConversationExecutionProfile::default(),
            selected_model_path: None,
            source_conversation_id: None,
            source_message_id: None,
            branch_root_message_id: None,
            active_leaf_message_id: None,
            recipient_ids: Vec::new(),
            current_skill_ids: Vec::new(),
            messages: Vec::new(),
        }
    }

    #[cfg(unix)]
    #[test]
    fn rename_preserves_committed_replies_other_chats_selection_and_draft() {
        with_rename_store(|store| {
            let mut db = super::ConversationDb {
                conversations: vec![rename_fixture("host")],
                selected_conversation_id: Some("host".into()),
            };
            store.put(super::CONVERSATIONS_NAMESPACE, &db)?;
            // Work committed since the sidebar displayed the old title must
            // survive. Reopen the real store to verify durable preservation.
            db.conversations[0].messages = vec![
                message("user", None, MessageRole::User, "Preserve my input"),
                message(
                    "reply",
                    Some("user"),
                    MessageRole::Assistant,
                    "Native reply",
                ),
            ];
            db.conversations[0].active_leaf_message_id = Some("reply".into());
            db.conversations.push(rename_fixture("other"));
            db.selected_conversation_id = Some("other".into());
            store.put(super::CONVERSATIONS_NAMESPACE, &db)?;
            let draft = super::DraftDb {
                drafts: vec![super::DraftMessage {
                    conversation_id: Some("default".into()),
                    message: "Unsent input".into(),
                    attachment_ids: Vec::new(),
                    updated_at: "2".into(),
                }],
            };
            store.put(super::DRAFTS_NAMESPACE, &draft)?;
            let result = super::rename_conversation_in_store(store, "host", "  Chosen title  ")?
                .expect("existing conversation");
            assert_eq!(result.title, "Chosen title");
            assert_eq!(result.messages, db.conversations[0].messages);
            db.conversations[0].title = result.title;
            db.conversations[0].updated_at = result.updated_at;
            let reopened =
                crate::store::RuntimeStore::open(store.path().parent().expect("store parent"))?;
            assert_eq!(
                reopened.get::<super::ConversationDb>(super::CONVERSATIONS_NAMESPACE)?,
                Some(db)
            );
            assert_eq!(
                reopened.get::<super::DraftDb>(super::DRAFTS_NAMESPACE)?,
                Some(draft)
            );
            Ok(())
        });
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_rename_never_discards_committed_messages() {
        with_rename_store(|store| {
            store.put(
                super::CONVERSATIONS_NAMESPACE,
                &super::ConversationDb {
                    conversations: vec![rename_fixture("host")],
                    selected_conversation_id: Some("host".into()),
                },
            )?;
            let start = std::sync::Barrier::new(2);
            std::thread::scope(|threads| {
                let writer = threads.spawn(|| -> anyhow::Result<()> {
                    start.wait();
                    for turn in 0..64 {
                        store.mutate_documents(
                            super::CONVERSATIONS_NAMESPACE,
                            super::ConversationDb::default,
                            |db, _| {
                                let host = &mut db.conversations[0];
                                let user_id = format!("user-{turn}");
                                let reply_id = format!("reply-{turn}");
                                let mut user = message(
                                    &user_id,
                                    host.active_leaf_message_id.as_deref(),
                                    MessageRole::User,
                                    "User input",
                                );
                                let mut reply = message(
                                    &reply_id,
                                    Some(&user_id),
                                    MessageRole::Assistant,
                                    "Committed reply",
                                );
                                user.conversation_id = "host".into();
                                reply.conversation_id = "host".into();
                                host.messages.extend([user, reply]);
                                host.active_leaf_message_id = Some(reply_id);
                                Ok(())
                            },
                        )?;
                    }
                    Ok(())
                });
                let renamer = threads.spawn(|| -> anyhow::Result<()> {
                    start.wait();
                    for iteration in 0..64 {
                        assert!(
                            super::rename_conversation_in_store(
                                store,
                                "host",
                                &format!("Title {iteration}")
                            )?
                            .is_some()
                        );
                    }
                    Ok(())
                });
                writer.join().expect("writer thread")?;
                renamer.join().expect("rename thread")?;
                Ok::<_, anyhow::Error>(())
            })?;
            let db = store
                .get::<super::ConversationDb>(super::CONVERSATIONS_NAMESPACE)?
                .expect("retained conversations");
            assert_eq!(db.conversations[0].messages.len(), 128);
            assert_eq!(
                db.conversations[0].active_leaf_message_id.as_deref(),
                Some("reply-63")
            );
            Ok(())
        });
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_selection_never_discards_committed_messages() {
        with_rename_store(|store| {
            store.put(
                super::CONVERSATIONS_NAMESPACE,
                &super::ConversationDb {
                    conversations: vec![rename_fixture("host")],
                    selected_conversation_id: Some("host".into()),
                },
            )?;
            let start = std::sync::Barrier::new(2);
            std::thread::scope(|threads| {
                let writer = threads.spawn(|| -> anyhow::Result<()> {
                    start.wait();
                    for turn in 0..64 {
                        store.mutate_documents(
                            super::CONVERSATIONS_NAMESPACE,
                            super::ConversationDb::default,
                            |db, _| {
                                let host = &mut db.conversations[0];
                                let user_id = format!("user-{turn}");
                                let reply_id = format!("reply-{turn}");
                                let mut user = message(
                                    &user_id,
                                    host.active_leaf_message_id.as_deref(),
                                    MessageRole::User,
                                    "User input",
                                );
                                let mut reply = message(
                                    &reply_id,
                                    Some(&user_id),
                                    MessageRole::Assistant,
                                    "Committed reply",
                                );
                                user.conversation_id = "host".into();
                                reply.conversation_id = "host".into();
                                host.messages.extend([user, reply]);
                                host.active_leaf_message_id = Some(reply_id);
                                Ok(())
                            },
                        )?;
                    }
                    Ok(())
                });
                let renamer = threads.spawn(|| -> anyhow::Result<()> {
                    start.wait();
                    for _ in 0..64 {
                        assert!(super::select_conversation_in_store(store, "host")?.is_some());
                    }
                    Ok(())
                });
                writer.join().expect("writer thread")?;
                renamer.join().expect("rename thread")?;
                Ok::<_, anyhow::Error>(())
            })?;
            let db = store
                .get::<super::ConversationDb>(super::CONVERSATIONS_NAMESPACE)?
                .expect("retained conversations");
            assert_eq!(db.conversations[0].messages.len(), 128);
            assert_eq!(
                db.conversations[0].active_leaf_message_id.as_deref(),
                Some("reply-63")
            );
            Ok(())
        });
    }

    #[cfg(unix)]
    #[test]
    fn rename_does_not_resurrect_a_removed_conversation() {
        with_rename_store(|store| {
            let original = super::ConversationDb {
                conversations: vec![rename_fixture("host")],
                selected_conversation_id: Some("host".into()),
            };
            store.put(super::CONVERSATIONS_NAMESPACE, &original)?;
            let current = super::ConversationDb {
                conversations: vec![rename_fixture("other")],
                selected_conversation_id: Some("other".into()),
            };
            store.put(super::CONVERSATIONS_NAMESPACE, &current)?;
            assert!(super::rename_conversation_in_store(store, "host", "Gone")?.is_none());
            assert_eq!(
                store.get::<super::ConversationDb>(super::CONVERSATIONS_NAMESPACE)?,
                Some(current)
            );
            Ok(())
        });
    }

    #[cfg(unix)]
    #[test]
    fn rename_retains_the_existing_blank_title_policy() {
        with_rename_store(|store| {
            let db = super::ConversationDb {
                conversations: vec![rename_fixture("host")],
                selected_conversation_id: Some("host".into()),
            };
            store.put(super::CONVERSATIONS_NAMESPACE, &db)?;
            let result = super::rename_conversation_in_store(store, "host", " \n\t ")?
                .expect("existing conversation");
            assert_eq!(result.title, "Untitled conversation");
            Ok(())
        });
    }
}
