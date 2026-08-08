use crate::config::{resolve_settings, upstream_setting_string};
use crate::consult::consult_panel_list;
use crate::conversation_store::{
    ChatTemplatePolicy, Conversation, ConversationDb, ConversationExecutionProfile,
    ConversationKind, Message, MessageRole, ToolBinding, active_path_messages, load_db, save_db,
};
use crate::native_runtime::resident_model_for_profile;
use crate::now_ms;
use crate::persona_library::{LIBRARY_REVISION, builtin_personas};
use crate::receipts::{Blocker, CommandResult};
use crate::store::RuntimeStore;
use anyhow::Result;
use llama_native_types::{ChatMessage, ChatRole, ChatTemplateChoice};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use uuid::Uuid;

const GROUPS_NAMESPACE: &str = "persona-groups.v1";
const PERSONA_VERSIONS_NAMESPACE: &str = "persona-versions.v1";
const MAX_GROUP_MEMBERS: usize = 4;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PersonaHistoryMode {
    Full,
    SystemOnly,
    Empty,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersonaFreezeInput {
    pub conversation_id: String,
    pub message_id: String,
    pub name: String,
    pub mention_handle: String,
    pub history_mode: PersonaHistoryMode,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PersonaUpdateInput {
    pub persona_id: String,
    pub name: String,
    pub mention_handle: String,
    pub model_path: Option<PathBuf>,
    pub mmproj_path: Option<PathBuf>,
    pub system_message: Option<String>,
    pub sampling: Option<llama_native_types::SamplingConfig>,
    pub chat_template: ChatTemplatePolicy,
    pub tool_bindings: Vec<ToolBinding>,
    pub source_history_tokens: u32,
    pub host_context_tokens: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersonaGroup {
    pub id: String,
    pub name: String,
    pub mention_handle: String,
    pub persona_ids: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
struct PersonaGroupDb {
    groups: Vec<PersonaGroup>,
    #[serde(default)]
    legacy_consult_migrated: bool,
    #[serde(default)]
    catalog_revision: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersonaVersion {
    pub persona_id: String,
    pub version: u64,
    pub profile_sha256: String,
    pub conversation_sha256: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
struct PersonaVersionDb {
    versions: Vec<PersonaVersion>,
}

pub fn persona_freeze(input: PersonaFreezeInput) -> Result<CommandResult<Conversation>> {
    let mut db = load_db()?;
    ensure_handles(&mut db);
    let Some(source) = db
        .conversations
        .iter()
        .find(|conversation| conversation.id == input.conversation_id)
        .cloned()
    else {
        return Ok(blocked_persona(
            "mom_llama.persona_freeze",
            "persona_source_not_found",
            "The source chat no longer exists.",
        ));
    };
    let active = active_path_messages(&source);
    let Some(index) = active
        .iter()
        .position(|message| message.id == input.message_id)
    else {
        return Ok(blocked_persona(
            "mom_llama.persona_freeze",
            "persona_source_not_on_active_branch",
            "Choose a message on the chat's active branch.",
        ));
    };
    let handle =
        match validate_available_handle(&db, None, &input.mention_handle, &load_group_db()?.groups)
        {
            Ok(handle) => handle,
            Err(blocker) => {
                return Ok(CommandResult::blocked(
                    "mom_llama.persona_freeze",
                    "stub_blocked",
                    blocker,
                ));
            }
        };
    let settings = resolve_settings()?;
    let persona_id = Uuid::new_v4().to_string();
    let selected = match input.history_mode {
        PersonaHistoryMode::Full => active[..=index].to_vec(),
        PersonaHistoryMode::SystemOnly => active[..=index]
            .iter()
            .filter(|message| message.role == MessageRole::System)
            .cloned()
            .collect(),
        PersonaHistoryMode::Empty => Vec::new(),
    };
    let messages = remap_messages(&persona_id, selected);
    let mut profile = source.execution_profile.clone();
    profile.mention_handle = handle;
    profile.model_path = profile
        .model_path
        .or_else(|| source.selected_model_path.clone())
        .or_else(|| settings.model_path.clone());
    profile.mmproj_path = profile.mmproj_path.or(settings.mmproj_path.clone());
    if profile.system_message.is_none() {
        profile.system_message = upstream_setting_string(&settings, "systemMessage")
            .filter(|message| !message.trim().is_empty());
    }
    profile.version = 1;
    let now = now_ms().to_string();
    let persona = Conversation {
        id: persona_id.clone(),
        title: clean_name(&input.name, "New persona"),
        created_at: now.clone(),
        updated_at: now,
        kind: ConversationKind::PersonaTemplate,
        execution_profile: profile.clone(),
        selected_model_path: profile.model_path.clone(),
        source_conversation_id: Some(source.id),
        source_message_id: Some(input.message_id.clone()),
        branch_root_message_id: Some(input.message_id),
        active_leaf_message_id: messages.last().map(|message| message.id.clone()),
        current_skill_ids: source.current_skill_ids,
        messages,
    };
    db.conversations.insert(0, persona.clone());
    let path = save_db(&db)?;
    record_persona_version(&persona)?;
    Ok(CommandResult::passed(
        "mom_llama.persona_freeze",
        "contracted",
        persona,
        vec![path.display().to_string()],
        Vec::new(),
        false,
        false,
    ))
}

pub fn persona_list() -> Result<CommandResult<Vec<Conversation>>> {
    migrate_legacy_consult()?;
    let mut db = load_db()?;
    let changed = ensure_handles(&mut db);
    if changed {
        save_db(&db)?;
    }
    let personas = db
        .conversations
        .into_iter()
        .filter(|conversation| conversation.kind == ConversationKind::PersonaTemplate)
        .collect();
    Ok(CommandResult::passed(
        "mom_llama.persona_list",
        "contracted",
        personas,
        Vec::new(),
        Vec::new(),
        false,
        false,
    ))
}

pub fn persona_get(persona_id: &str) -> Result<CommandResult<Conversation>> {
    let db = load_db()?;
    let Some(persona) = db.conversations.into_iter().find(|conversation| {
        conversation.id == persona_id && conversation.kind == ConversationKind::PersonaTemplate
    }) else {
        return Ok(blocked_persona(
            "mom_llama.persona_get",
            "persona_not_found",
            "The persona no longer exists.",
        ));
    };
    Ok(CommandResult::passed(
        "mom_llama.persona_get",
        "contracted",
        persona,
        Vec::new(),
        Vec::new(),
        false,
        false,
    ))
}

pub fn persona_update(input: PersonaUpdateInput) -> Result<CommandResult<Conversation>> {
    let mut db = load_db()?;
    let groups = load_group_db()?.groups;
    let handle = match validate_available_handle(
        &db,
        Some(&input.persona_id),
        &input.mention_handle,
        &groups,
    ) {
        Ok(handle) => handle,
        Err(blocker) => {
            return Ok(CommandResult::blocked(
                "mom_llama.persona_update",
                "stub_blocked",
                blocker,
            ));
        }
    };
    if let ChatTemplatePolicy::FrozenSource(template) = &input.chat_template {
        if template.trim().is_empty() {
            return Ok(CommandResult::blocked(
                "mom_llama.persona_update",
                "stub_blocked",
                Blocker::new(
                    "persona_chat_template_empty",
                    "A frozen chat template cannot be empty.",
                    vec!["Choose the model default or provide a complete template.".to_string()],
                ),
            ));
        }
        let settings = resolve_settings()?;
        let Some(model_path) = input
            .model_path
            .as_deref()
            .or(settings.model_path.as_deref())
        else {
            return Ok(CommandResult::blocked(
                "mom_llama.persona_update",
                "blocked_missing_model",
                Blocker::new(
                    "persona_template_model_missing",
                    "Select a model before freezing a custom chat template.",
                    vec!["Choose this Persona's GGUF model.".to_string()],
                ),
            ));
        };
        let model =
            match resident_model_for_profile(&settings, model_path, input.mmproj_path.as_deref()) {
                Ok(model) => model,
                Err(blocked) => {
                    return Ok(CommandResult::blocked(
                        "mom_llama.persona_update",
                        &blocked.readiness,
                        blocked.blocker,
                    ));
                }
            };
        if let Err(error) = model.tokenize_messages_with_template(
            vec![ChatMessage {
                role: ChatRole::User,
                content: "Template validation".to_string(),
            }],
            ChatTemplateChoice::Override(template.clone()),
        ) {
            return Ok(CommandResult::blocked(
                "mom_llama.persona_update",
                "blocked_invalid_model",
                Blocker::new(
                    "persona_chat_template_invalid",
                    format!(
                        "The selected model rejected this chat template: {}",
                        error.message
                    ),
                    vec!["Correct the template or use the model default.".to_string()],
                ),
            ));
        }
    }
    let Some(persona) = db.conversations.iter_mut().find(|conversation| {
        conversation.id == input.persona_id
            && conversation.kind == ConversationKind::PersonaTemplate
    }) else {
        return Ok(blocked_persona(
            "mom_llama.persona_update",
            "persona_not_found",
            "The persona no longer exists.",
        ));
    };
    persona.title = clean_name(&input.name, "Persona");
    persona.execution_profile = ConversationExecutionProfile {
        mention_handle: handle,
        model_path: input.model_path.clone(),
        mmproj_path: input.mmproj_path,
        system_message: input
            .system_message
            .filter(|value| !value.trim().is_empty()),
        sampling: input.sampling,
        chat_template: input.chat_template,
        tool_bindings: normalize_tools(input.tool_bindings),
        source_history_tokens: input.source_history_tokens.clamp(0, 32768),
        host_context_tokens: input.host_context_tokens.clamp(0, 32768),
        version: persona.execution_profile.version.saturating_add(1),
    };
    persona.selected_model_path = input.model_path;
    persona.updated_at = now_ms().to_string();
    let output = persona.clone();
    let path = save_db(&db)?;
    record_persona_version(&output)?;
    Ok(CommandResult::passed(
        "mom_llama.persona_update",
        "contracted",
        output,
        vec![path.display().to_string()],
        Vec::new(),
        false,
        false,
    ))
}

pub(crate) fn record_persona_version(persona: &Conversation) -> Result<PersonaVersion> {
    if persona.kind != ConversationKind::PersonaTemplate {
        anyhow::bail!("only Persona templates have Persona version records");
    }
    let profile_sha256 = sha256_json(&persona.execution_profile)?;
    let conversation_sha256 = sha256_json(&(
        &persona.title,
        &persona.execution_profile,
        &persona.active_leaf_message_id,
        active_path_messages(persona),
    ))?;
    let version = PersonaVersion {
        persona_id: persona.id.clone(),
        version: persona.execution_profile.version,
        profile_sha256,
        conversation_sha256,
        created_at: now_ms().to_string(),
    };
    RuntimeStore::current()?.mutate(
        PERSONA_VERSIONS_NAMESPACE,
        PersonaVersionDb::default,
        |db| {
            db.versions.retain(|candidate| {
                candidate.persona_id != version.persona_id || candidate.version != version.version
            });
            db.versions.push(version.clone());
            Ok(())
        },
    )?;
    Ok(version)
}

pub fn persona_versions(persona_id: &str) -> Result<Vec<PersonaVersion>> {
    let mut versions = RuntimeStore::current()?
        .get::<PersonaVersionDb>(PERSONA_VERSIONS_NAMESPACE)?
        .unwrap_or_default()
        .versions
        .into_iter()
        .filter(|version| version.persona_id == persona_id)
        .collect::<Vec<_>>();
    versions.sort_by_key(|version| version.version);
    Ok(versions)
}

fn sha256_json(value: &impl Serialize) -> Result<String> {
    use sha2::{Digest, Sha256};
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(value)?)))
}

pub fn persona_delete(persona_id: &str) -> Result<CommandResult<Conversation>> {
    let mut db = load_db()?;
    let Some(index) = db.conversations.iter().position(|conversation| {
        conversation.id == persona_id && conversation.kind == ConversationKind::PersonaTemplate
    }) else {
        return Ok(blocked_persona(
            "mom_llama.persona_delete",
            "persona_not_found",
            "The persona no longer exists.",
        ));
    };
    let removed = db.conversations.remove(index);
    let path = save_db(&db)?;
    let store = RuntimeStore::current()?;
    store.mutate(GROUPS_NAMESPACE, PersonaGroupDb::default, |groups| {
        for group in &mut groups.groups {
            group.persona_ids.retain(|id| id != persona_id);
        }
        Ok(())
    })?;
    Ok(CommandResult::passed(
        "mom_llama.persona_delete",
        "contracted",
        removed,
        vec![path.display().to_string()],
        Vec::new(),
        false,
        false,
    ))
}

pub fn persona_instantiate(
    persona_id: &str,
    title: Option<String>,
) -> Result<CommandResult<Conversation>> {
    let mut db = load_db()?;
    let Some(persona) = db
        .conversations
        .iter()
        .find(|conversation| {
            conversation.id == persona_id && conversation.kind == ConversationKind::PersonaTemplate
        })
        .cloned()
    else {
        return Ok(blocked_persona(
            "mom_llama.persona_instantiate",
            "persona_not_found",
            "The persona no longer exists.",
        ));
    };
    let id = Uuid::new_v4().to_string();
    let messages = remap_messages(&id, active_path_messages(&persona));
    let now = now_ms().to_string();
    let mut profile = persona.execution_profile.clone();
    profile.mention_handle = unique_handle(
        &db,
        &load_group_db()?.groups,
        &format!("{}-chat", persona.title),
    );
    profile.version = 1;
    let conversation = Conversation {
        id: id.clone(),
        title: title.unwrap_or_else(|| format!("Chat with {}", persona.title)),
        created_at: now.clone(),
        updated_at: now,
        kind: ConversationKind::Chat,
        execution_profile: profile.clone(),
        selected_model_path: profile.model_path.clone(),
        source_conversation_id: Some(persona.id),
        source_message_id: persona.active_leaf_message_id,
        branch_root_message_id: None,
        active_leaf_message_id: messages.last().map(|message| message.id.clone()),
        current_skill_ids: persona.current_skill_ids,
        messages,
    };
    db.selected_conversation_id = Some(id);
    db.conversations.insert(0, conversation.clone());
    let path = save_db(&db)?;
    Ok(CommandResult::passed(
        "mom_llama.persona_instantiate",
        "contracted",
        conversation,
        vec![path.display().to_string()],
        Vec::new(),
        false,
        false,
    ))
}

pub fn persona_group_list() -> Result<CommandResult<Vec<PersonaGroup>>> {
    migrate_legacy_consult()?;
    Ok(CommandResult::passed(
        "mom_llama.persona_group_list",
        "contracted",
        load_group_db()?.groups,
        Vec::new(),
        Vec::new(),
        false,
        false,
    ))
}

pub fn persona_group_create(
    name: String,
    mention_handle: String,
    persona_ids: Vec<String>,
) -> Result<CommandResult<PersonaGroup>> {
    write_group(
        None,
        name,
        mention_handle,
        persona_ids,
        "mom_llama.persona_group_create",
    )
}

pub fn persona_group_update(
    group_id: String,
    name: String,
    mention_handle: String,
    persona_ids: Vec<String>,
) -> Result<CommandResult<PersonaGroup>> {
    write_group(
        Some(group_id),
        name,
        mention_handle,
        persona_ids,
        "mom_llama.persona_group_update",
    )
}

pub fn persona_group_delete(group_id: &str) -> Result<CommandResult<PersonaGroup>> {
    let store = RuntimeStore::current()?;
    let mut removed = None;
    store.mutate(GROUPS_NAMESPACE, PersonaGroupDb::default, |db| {
        if let Some(index) = db.groups.iter().position(|group| group.id == group_id) {
            removed = Some(db.groups.remove(index));
        }
        Ok(())
    })?;
    let Some(group) = removed else {
        return Ok(CommandResult::blocked(
            "mom_llama.persona_group_delete",
            "stub_blocked",
            Blocker::new(
                "persona_group_not_found",
                "The consult group no longer exists.",
                vec!["Refresh Consult groups in Settings.".to_string()],
            ),
        ));
    };
    Ok(CommandResult::passed(
        "mom_llama.persona_group_delete",
        "contracted",
        group,
        vec![store.path().display().to_string()],
        Vec::new(),
        false,
        false,
    ))
}

pub(crate) fn conversation_and_group_handles() -> Result<(Vec<Conversation>, Vec<PersonaGroup>)> {
    migrate_legacy_consult()?;
    let mut db = load_db()?;
    if ensure_handles(&mut db) {
        save_db(&db)?;
    }
    Ok((db.conversations, load_group_db()?.groups))
}

fn write_group(
    group_id: Option<String>,
    name: String,
    mention_handle: String,
    persona_ids: Vec<String>,
    command: &str,
) -> Result<CommandResult<PersonaGroup>> {
    let mut conversation_db = load_db()?;
    ensure_handles(&mut conversation_db);
    let persona_ids = persona_ids
        .into_iter()
        .filter(|id| !id.trim().is_empty())
        .collect::<Vec<_>>();
    if persona_ids.is_empty() || persona_ids.len() > MAX_GROUP_MEMBERS {
        return Ok(CommandResult::blocked(
            command,
            "stub_blocked",
            Blocker::new(
                "persona_group_size_invalid",
                "A consult group needs between one and four personas.",
                vec!["Choose one to four frozen personas.".to_string()],
            ),
        ));
    }
    let unique = persona_ids.iter().collect::<BTreeSet<_>>();
    if unique.len() != persona_ids.len()
        || persona_ids.iter().any(|id| {
            !conversation_db.conversations.iter().any(|conversation| {
                conversation.id == *id && conversation.kind == ConversationKind::PersonaTemplate
            })
        })
    {
        return Ok(CommandResult::blocked(
            command,
            "stub_blocked",
            Blocker::new(
                "persona_group_member_invalid",
                "Every consult-group member must be a distinct frozen persona.",
                vec!["Refresh Personas and choose valid members.".to_string()],
            ),
        ));
    }
    let mut db = load_group_db()?;
    let handle = match validate_available_handle(
        &conversation_db,
        group_id.as_deref(),
        &mention_handle,
        &db.groups,
    ) {
        Ok(handle) => handle,
        Err(blocker) => return Ok(CommandResult::blocked(command, "stub_blocked", blocker)),
    };
    let now = now_ms().to_string();
    let id = group_id.unwrap_or_else(|| Uuid::new_v4().to_string());
    let created_at = db
        .groups
        .iter()
        .find(|group| group.id == id)
        .map(|group| group.created_at.clone())
        .unwrap_or_else(|| now.clone());
    let group = PersonaGroup {
        id: id.clone(),
        name: clean_name(&name, "Consult group"),
        mention_handle: handle,
        persona_ids,
        created_at,
        updated_at: now,
    };
    db.groups.retain(|candidate| candidate.id != id);
    db.groups.insert(0, group.clone());
    let store = RuntimeStore::current()?;
    store.put(GROUPS_NAMESPACE, &db)?;
    Ok(CommandResult::passed(
        command,
        "contracted",
        group,
        vec![store.path().display().to_string()],
        Vec::new(),
        false,
        false,
    ))
}

fn migrate_legacy_consult() -> Result<()> {
    let mut groups = load_group_db()?;
    if groups.legacy_consult_migrated
        && groups.catalog_revision.as_deref() == Some(LIBRARY_REVISION)
    {
        return Ok(());
    }
    let mut conversations = load_db()?;
    ensure_handles(&mut conversations);

    let settings = resolve_settings()?;
    let catalog = builtin_personas();
    let catalog_ids = catalog
        .iter()
        .map(|persona| format!("persona-{}", persona.id))
        .collect::<BTreeSet<_>>();

    // Replace the previous abstract-lens seed in place. These stable IDs were
    // owned by the application, so user-created Personas are never selected by
    // this migration. Existing group references remain valid.
    for source in catalog {
        let id = format!("persona-{}", source.id);
        let desired_handle = source.id.replace('_', "-");
        let handle = if conversations.conversations.iter().any(|conversation| {
            conversation.id != id
                && normalize_handle(&conversation.execution_profile.mention_handle)
                    == desired_handle
        }) {
            unique_handle(&conversations, &groups.groups, &source.label)
        } else {
            desired_handle
        };
        if let Some(persona) = conversations
            .conversations
            .iter_mut()
            .find(|conversation| conversation.id == id)
        {
            persona.title = source.label;
            persona.kind = ConversationKind::PersonaTemplate;
            persona.execution_profile.mention_handle = handle;
            persona.execution_profile.system_message = Some(source.perspective_prompt);
            persona.execution_profile.version = persona.execution_profile.version.saturating_add(1);
            persona.updated_at = now_ms().to_string();
        } else {
            let now = now_ms().to_string();
            conversations.conversations.push(Conversation {
                id,
                title: source.label,
                created_at: now.clone(),
                updated_at: now,
                kind: ConversationKind::PersonaTemplate,
                execution_profile: ConversationExecutionProfile {
                    mention_handle: handle,
                    model_path: settings.model_path.clone(),
                    mmproj_path: settings.mmproj_path.clone(),
                    system_message: Some(source.perspective_prompt),
                    ..ConversationExecutionProfile::default()
                },
                selected_model_path: settings.model_path.clone(),
                source_conversation_id: None,
                source_message_id: None,
                branch_root_message_id: None,
                active_leaf_message_id: None,
                current_skill_ids: Vec::new(),
                messages: Vec::new(),
            });
        }
    }

    // Default panel patterns are no longer product state. Consult groups are
    // explicitly configured in Settings. Preserve only user-created legacy
    // panels and remap their references onto the canonical Persona IDs.
    groups
        .groups
        .retain(|group| !group.id.starts_with("group-builtin-"));
    let panels = consult_panel_list()?.result.unwrap_or_default();
    for panel in panels
        .into_iter()
        .filter(|panel| !panel.id.starts_with("builtin-"))
    {
        let mut persona_ids = Vec::new();
        for legacy in panel.personas {
            let id = format!("persona-{}", legacy.id);
            if !conversations
                .conversations
                .iter()
                .any(|conversation| conversation.id == id)
            {
                let now = now_ms().to_string();
                let handle = unique_handle(&conversations, &groups.groups, &legacy.label);
                conversations.conversations.push(Conversation {
                    id: id.clone(),
                    title: legacy.label.clone(),
                    created_at: now.clone(),
                    updated_at: now,
                    kind: ConversationKind::PersonaTemplate,
                    execution_profile: ConversationExecutionProfile {
                        mention_handle: handle,
                        model_path: settings.model_path.clone(),
                        mmproj_path: settings.mmproj_path.clone(),
                        system_message: Some(legacy.perspective_prompt),
                        ..ConversationExecutionProfile::default()
                    },
                    selected_model_path: settings.model_path.clone(),
                    source_conversation_id: None,
                    source_message_id: None,
                    branch_root_message_id: None,
                    active_leaf_message_id: None,
                    current_skill_ids: Vec::new(),
                    messages: Vec::new(),
                });
            }
            persona_ids.push(id);
        }
        let group_id = format!("group-{}", panel.id);
        if !groups.groups.iter().any(|group| group.id == group_id) {
            let now = now_ms().to_string();
            let handle = unique_handle(&conversations, &groups.groups, &panel.name);
            groups.groups.push(PersonaGroup {
                id: group_id,
                name: panel.name,
                mention_handle: handle,
                persona_ids,
                created_at: now.clone(),
                updated_at: now,
            });
        }
    }
    groups.legacy_consult_migrated = true;
    groups.catalog_revision = Some(LIBRARY_REVISION.to_string());
    save_db(&conversations)?;
    for persona in conversations.conversations.iter().filter(|conversation| {
        conversation.kind == ConversationKind::PersonaTemplate
            && (catalog_ids.contains(&conversation.id)
                || conversation.source_conversation_id.is_none())
    }) {
        record_persona_version(persona)?;
    }
    RuntimeStore::current()?.put(GROUPS_NAMESPACE, &groups)?;
    Ok(())
}

fn load_group_db() -> Result<PersonaGroupDb> {
    Ok(RuntimeStore::current()?
        .get(GROUPS_NAMESPACE)?
        .unwrap_or_default())
}

fn ensure_handles(db: &mut ConversationDb) -> bool {
    let mut used = db
        .conversations
        .iter()
        .filter_map(|conversation| {
            let handle = normalize_handle(&conversation.execution_profile.mention_handle);
            (!handle.is_empty()).then_some(handle)
        })
        .collect::<BTreeSet<_>>();
    let mut changed = false;
    for conversation in &mut db.conversations {
        if conversation
            .execution_profile
            .mention_handle
            .trim()
            .is_empty()
        {
            let base = slug(&conversation.title);
            let handle = unique_from_used(&mut used, &base);
            conversation.execution_profile.mention_handle = handle;
            changed = true;
        }
        if conversation.execution_profile.model_path.is_none() {
            conversation.execution_profile.model_path = conversation.selected_model_path.clone();
        }
    }
    changed
}

fn validate_available_handle(
    conversations: &ConversationDb,
    current_id: Option<&str>,
    value: &str,
    groups: &[PersonaGroup],
) -> Result<String, Blocker> {
    let handle = normalize_handle(value);
    if handle.len() < 2
        || handle.len() > 48
        || !handle.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
        })
    {
        return Err(Blocker::new(
            "mention_handle_invalid",
            "Handles use 2–48 lowercase letters, numbers, or hyphens.",
            vec!["Choose a handle such as `evidence-lens`.".to_string()],
        ));
    }
    let conversation_taken = conversations.conversations.iter().any(|conversation| {
        Some(conversation.id.as_str()) != current_id
            && normalize_handle(&conversation.execution_profile.mention_handle) == handle
    });
    let group_taken = groups.iter().any(|group| {
        Some(group.id.as_str()) != current_id && normalize_handle(&group.mention_handle) == handle
    });
    if conversation_taken || group_taken {
        return Err(Blocker::new(
            "mention_handle_taken",
            format!("The handle `@{handle}` is already in use."),
            vec!["Choose another handle.".to_string()],
        ));
    }
    Ok(handle)
}

fn unique_handle(db: &ConversationDb, groups: &[PersonaGroup], value: &str) -> String {
    let mut used = db
        .conversations
        .iter()
        .map(|conversation| normalize_handle(&conversation.execution_profile.mention_handle))
        .chain(
            groups
                .iter()
                .map(|group| normalize_handle(&group.mention_handle)),
        )
        .filter(|handle| !handle.is_empty())
        .collect::<BTreeSet<_>>();
    unique_from_used(&mut used, &slug(value))
}

fn unique_from_used(used: &mut BTreeSet<String>, base: &str) -> String {
    let base = if base.len() < 2 { "chat" } else { base };
    let mut candidate = base.to_string();
    let mut suffix = 2;
    while used.contains(&candidate) {
        candidate = format!("{base}-{suffix}");
        suffix += 1;
    }
    used.insert(candidate.clone());
    candidate
}

fn normalize_handle(value: &str) -> String {
    value.trim().trim_start_matches('@').to_ascii_lowercase()
}

fn slug(value: &str) -> String {
    let mut output = String::new();
    let mut hyphen = false;
    for character in value.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_alphanumeric() {
            output.push(character);
            hyphen = false;
        } else if !output.is_empty() && !hyphen {
            output.push('-');
            hyphen = true;
        }
    }
    output.trim_matches('-').chars().take(48).collect()
}

fn remap_messages(conversation_id: &str, messages: Vec<Message>) -> Vec<Message> {
    let ids = messages
        .iter()
        .map(|message| (message.id.clone(), Uuid::new_v4().to_string()))
        .collect::<HashMap<_, _>>();
    messages
        .into_iter()
        .map(|mut message| {
            message.id = ids
                .get(&message.id)
                .cloned()
                .unwrap_or_else(|| Uuid::new_v4().to_string());
            message.parent_id = message
                .parent_id
                .as_ref()
                .and_then(|parent| ids.get(parent).cloned());
            message.conversation_id = conversation_id.to_string();
            message.branch_index = None;
            message.branch_count = None;
            message.attribution = None;
            message
        })
        .collect()
}

fn normalize_tools(tools: Vec<ToolBinding>) -> Vec<ToolBinding> {
    let mut seen = BTreeSet::new();
    tools
        .into_iter()
        .filter_map(|tool| {
            let tool = ToolBinding {
                server: tool.server.trim().to_string(),
                tool: tool.tool.trim().to_string(),
            };
            (!tool.server.is_empty()
                && !tool.tool.is_empty()
                && seen.insert((tool.server.clone(), tool.tool.clone())))
            .then_some(tool)
        })
        .collect()
}

fn clean_name(value: &str, fallback: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        fallback.to_string()
    } else {
        value.chars().take(96).collect()
    }
}

fn blocked_persona(command: &str, code: &str, message: &str) -> CommandResult<Conversation> {
    CommandResult::blocked(
        command,
        "stub_blocked",
        Blocker::new(
            code,
            message,
            vec!["Refresh Personas in Settings.".to_string()],
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::{normalize_handle, slug};

    #[test]
    fn mention_handles_are_stable_and_human_readable() {
        assert_eq!(normalize_handle("@Evidence-Lens"), "evidence-lens");
        assert_eq!(slug("  Whole-person lens  "), "whole-person-lens");
    }
}
