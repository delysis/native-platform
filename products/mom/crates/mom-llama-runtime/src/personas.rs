use crate::config::{resolve_settings, upstream_setting_string};
use crate::consult::{ConsultPanel, ConsultPersona, stored_legacy_panels};
use crate::conversation_store::{
    CONVERSATIONS_NAMESPACE, ChatTemplatePolicy, Conversation, ConversationDb,
    ConversationExecutionProfile, ConversationKind, DRAFTS_NAMESPACE, DraftDb, Message,
    MessageRole, ToolBinding, active_path_messages, load_db, save_db,
};
use crate::native_runtime::resident_model_for_profile;
use crate::now_ms;
use crate::operation_scope::OperationScope;
use crate::persona_library::{LIBRARY_REVISION, builtin_panels, builtin_personas};
use crate::receipts::{Blocker, CommandResult};
use crate::store::{DocumentMutations, DocumentSnapshot, RuntimeStore};
use anyhow::Result;
use fs2::FileExt;
use llama_native_types::{ChatMessage, ChatRole, ChatTemplateChoice};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::fs::{File, OpenOptions};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};
use uuid::Uuid;

const GROUPS_NAMESPACE: &str = "persona-groups.v1";
const PERSONA_VERSIONS_NAMESPACE: &str = "persona-versions.v1";
const PERSONA_REMOVALS_NAMESPACE: &str = "persona-removals.v1";
const PERSONA_REMOVAL_SCHEMA: &str = "mom_llama.persona_removal_impact.v1";
const PERSONA_CACHE_AUTHORITY_LOCK_FILE: &str = "persona-cache-authority.lock";
const MAX_GROUP_MEMBERS: usize = 4;
pub(crate) const MAX_PERSONA_TOOL_BINDINGS: usize = 8;
const PERSONA_STATE_MIGRATION_VERSION: u32 = 4;

static PERSONA_CACHE_AUTHORITY: OnceLock<Mutex<()>> = OnceLock::new();

pub(crate) struct PersonaCacheAuthorityGuard {
    _process_guard: MutexGuard<'static, ()>,
    file: File,
}

impl Drop for PersonaCacheAuthorityGuard {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

pub(crate) struct PersonaCacheOwnerLease {
    _authority: PersonaCacheAuthorityGuard,
    store: RuntimeStore,
    owner_id: String,
    removal_generation: u64,
}

impl PersonaCacheOwnerLease {
    pub(crate) const fn owner_generation(&self) -> u64 {
        self.removal_generation
    }

    pub(crate) fn validate(&self) -> Result<bool> {
        Ok(
            persona_cache_owner_removal_generation(&self.store, &self.owner_id)?
                == self.removal_generation
                && self.removal_generation == 0,
        )
    }
}

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
    #[serde(default)]
    pub auto_discover_mmproj: bool,
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
    migration_version: u32,
    #[serde(default)]
    catalog_revision: Option<String>,
    #[serde(default)]
    builtin_persona_provenance: Vec<BuiltinPersonaProvenance>,
}

impl PersonaGroupDb {
    fn legacy_consult_migration_is_current(&self) -> bool {
        self.legacy_consult_migrated
            && self.migration_version >= PERSONA_STATE_MIGRATION_VERSION
            && self.catalog_revision.as_deref() == Some(LIBRARY_REVISION)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum BuiltinPersonaOwnership {
    CatalogManaged,
    UserModified,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct BuiltinPersonaProvenance {
    persona_id: String,
    ownership: BuiltinPersonaOwnership,
    catalog_revision: String,
    catalog_entry_sha256: String,
    #[serde(default)]
    last_applied_persona_sha256: Option<String>,
    observed_persona_sha256: String,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersonaRemovalCommitInput {
    pub persona_id: String,
    pub persona_version: u64,
    pub impact_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersonaRemovalGroupImpact {
    pub group_id: String,
    pub group_name: String,
    pub member_index: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersonaRemovalDraftImpact {
    pub updated_at: String,
    pub message_sha256: String,
    pub attachment_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersonaRemovalAttachmentImpact {
    pub draft_attachment_ids: Vec<String>,
    pub removed_draft_only_unshared_attachment_ids: Vec<String>,
    pub retained_supporting_attachment_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersonaRemovalCacheImpact {
    pub owner_id: String,
    pub mom_persistent_cache_ids: Vec<String>,
    pub native_persistent_cache_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersonaRemovalHistoryImpact {
    pub retained_message_ids: Vec<String>,
    pub retained_persona_versions: Vec<u64>,
    pub retained_invocation_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersonaRemovalImpact {
    pub schema: String,
    pub persona_id: String,
    pub persona_version: u64,
    pub persona_title: String,
    pub persona_snapshot_sha256: String,
    pub groups: Vec<PersonaRemovalGroupImpact>,
    pub drafts: Vec<PersonaRemovalDraftImpact>,
    pub attachments: PersonaRemovalAttachmentImpact,
    pub caches: PersonaRemovalCacheImpact,
    pub active_invocation_ids: Vec<String>,
    pub retained_history: PersonaRemovalHistoryImpact,
    pub impact_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersonaRemovalOutput {
    pub impact: PersonaRemovalImpact,
    pub removed_at: String,
    pub already_removed: bool,
    pub evicted_memory_cache_entries: usize,
    pub evicted_native_live_cache_entries: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
struct PersonaRemovalLedger {
    removals: Vec<PersonaRemovalRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct PersonaRemovalRecord {
    persona_id: String,
    persona_version: Option<u64>,
    impact_sha256: String,
    impact: Option<PersonaRemovalImpact>,
    removed_at: String,
    #[serde(default)]
    persona_snapshot: Option<Conversation>,
    #[serde(default)]
    migration_tombstone: bool,
}

enum PersonaRemovalAttempt {
    Committed {
        impact: PersonaRemovalImpact,
        removed_at: String,
    },
    AlreadyCommitted {
        impact: PersonaRemovalImpact,
        removed_at: String,
    },
    Blocked(Blocker),
}

pub fn persona_freeze(input: PersonaFreezeInput) -> Result<CommandResult<Conversation>> {
    migrate_legacy_consult()?;
    let mut db = load_db()?;
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
    let mut profile = source.execution_profile.clone();
    profile.tool_bindings = match normalize_tools(profile.tool_bindings) {
        Ok(tools) => tools,
        Err(blocker) => {
            return Ok(CommandResult::blocked(
                "mom_llama.persona_freeze",
                "stub_blocked",
                blocker,
            ));
        }
    };
    let handle = match validate_available_handle(
        &db,
        None,
        None,
        &input.mention_handle,
        &load_group_db()?.groups,
    ) {
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
    let mut messages = remap_messages(&persona_id, selected);
    crate::attachments::snapshot_message_attachments(&persona_id, &mut messages)?;
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
    let path = save_persona_with_version(&db, &persona)?;
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
    let db = load_db()?;
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
    migrate_legacy_consult()?;
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
    let PersonaUpdateInput {
        persona_id,
        name,
        mention_handle,
        model_path,
        mut mmproj_path,
        auto_discover_mmproj,
        system_message,
        sampling,
        chat_template,
        tool_bindings,
        source_history_tokens,
        host_context_tokens,
    } = input;
    if auto_discover_mmproj {
        if let Some(model_path) = model_path.as_deref()
            && let Err(blocked) = crate::engine::validate_model_path(model_path)
        {
            return Ok(CommandResult::blocked(
                "mom_llama.persona_update",
                &blocked.readiness,
                blocked.blocker,
            ));
        }
        if mmproj_path.is_none()
            && let Some(model_path) = model_path.as_deref()
        {
            mmproj_path = match crate::models::discover_projector_for_model(model_path) {
                Ok(projector) => projector,
                Err(blocked) => {
                    return Ok(CommandResult::blocked(
                        "mom_llama.persona_update",
                        &blocked.readiness,
                        blocked.blocker,
                    ));
                }
            };
        }
    }
    let tool_bindings = match normalize_tools(tool_bindings) {
        Ok(tools) => tools,
        Err(blocker) => {
            return Ok(CommandResult::blocked(
                "mom_llama.persona_update",
                "stub_blocked",
                blocker,
            ));
        }
    };
    migrate_legacy_consult()?;
    if let ChatTemplatePolicy::FrozenSource(template) = &chat_template {
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
        let Some(model_path) = model_path.as_deref().or(settings.model_path.as_deref()) else {
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
        let model = match resident_model_for_profile(&settings, model_path, mmproj_path.as_deref())
        {
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
    let store = RuntimeStore::current()?;
    let updated = store.mutate_documents(
        CONVERSATIONS_NAMESPACE,
        ConversationDb::default,
        |conversations, documents| {
            if persona_cache_owner_is_removed_from_documents(documents, &persona_id)? {
                return Ok(Err(Blocker::new(
                    "persona_not_found",
                    "The Persona is no longer discoverable in the library.",
                    vec!["Refresh the Persona library.".to_string()],
                )));
            }
            let groups = documents
                .get::<PersonaGroupDb>(GROUPS_NAMESPACE)?
                .unwrap_or_default();
            let handle = match validate_available_handle(
                conversations,
                Some(&persona_id),
                None,
                &mention_handle,
                &groups.groups,
            ) {
                Ok(handle) => handle,
                Err(blocker) => return Ok(Err(blocker)),
            };
            let Some(persona) = conversations.conversations.iter_mut().find(|conversation| {
                conversation.id == persona_id
                    && conversation.kind == ConversationKind::PersonaTemplate
            }) else {
                return Ok(Err(Blocker::new(
                    "persona_not_found",
                    "The Persona no longer exists.",
                    vec!["Refresh the Persona library.".to_string()],
                )));
            };
            persona.title = clean_name(&name, "Persona");
            persona.execution_profile = ConversationExecutionProfile {
                mention_handle: handle,
                model_path: model_path.clone(),
                mmproj_path: mmproj_path.clone(),
                system_message: system_message
                    .clone()
                    .filter(|value| !value.trim().is_empty()),
                sampling: sampling.clone(),
                chat_template: chat_template.clone(),
                tool_bindings: tool_bindings.clone(),
                source_history_tokens: source_history_tokens.clamp(0, 32768),
                host_context_tokens: host_context_tokens.clamp(0, 32768),
                version: persona.execution_profile.version.saturating_add(1),
            };
            persona.selected_model_path = model_path.clone();
            persona.updated_at = now_ms().to_string();
            let output = persona.clone();
            let version = build_persona_version(&output)?;
            let mut versions = documents
                .get::<PersonaVersionDb>(PERSONA_VERSIONS_NAMESPACE)?
                .unwrap_or_default();
            versions.versions.retain(|candidate| {
                candidate.persona_id != version.persona_id || candidate.version != version.version
            });
            versions.versions.push(version);
            documents.put_bytes(PERSONA_VERSIONS_NAMESPACE, &serde_json::to_vec(&versions)?)?;
            Ok(Ok(output))
        },
    )?;
    let output = match updated {
        Ok(output) => output,
        Err(blocker) => {
            return Ok(CommandResult::blocked(
                "mom_llama.persona_update",
                "stub_blocked",
                blocker,
            ));
        }
    };
    Ok(CommandResult::passed(
        "mom_llama.persona_update",
        "contracted",
        output,
        vec![store.path().display().to_string()],
        Vec::new(),
        false,
        false,
    ))
}

pub(crate) fn record_persona_version(persona: &Conversation) -> Result<PersonaVersion> {
    let version = build_persona_version(persona)?;
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

pub(crate) fn save_persona_with_version(
    conversations: &ConversationDb,
    persona: &Conversation,
) -> Result<PathBuf> {
    let version = build_persona_version(persona)?;
    let store = RuntimeStore::current()?;
    store.mutate_documents(
        CONVERSATIONS_NAMESPACE,
        ConversationDb::default,
        |stored, documents| {
            let next = conversations.clone();
            reject_removed_conversation_writes_from_documents(&next, documents)?;
            if !next.conversations.iter().any(|conversation| {
                conversation.id == persona.id
                    && conversation.kind == ConversationKind::PersonaTemplate
                    && conversation.execution_profile.version == version.version
            }) {
                anyhow::bail!("Persona changed or was removed before its version could commit");
            }
            *stored = next;
            let mut versions = documents
                .get::<PersonaVersionDb>(PERSONA_VERSIONS_NAMESPACE)?
                .unwrap_or_default();
            versions.versions.retain(|candidate| {
                candidate.persona_id != version.persona_id || candidate.version != version.version
            });
            versions.versions.push(version.clone());
            documents.put_bytes(PERSONA_VERSIONS_NAMESPACE, &serde_json::to_vec(&versions)?)?;
            Ok(())
        },
    )?;
    Ok(store.path().to_path_buf())
}

fn build_persona_version(persona: &Conversation) -> Result<PersonaVersion> {
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

pub fn persona_removal_preview(persona_id: &str) -> Result<CommandResult<PersonaRemovalImpact>> {
    let scope = OperationScope::for_current_product_host();
    persona_removal_preview_in_scope(&scope, persona_id)
}

pub fn persona_removal_preview_in_scope(
    scope: &OperationScope,
    persona_id: &str,
) -> Result<CommandResult<PersonaRemovalImpact>> {
    const COMMAND: &str = "mom_llama.persona_removal_preview";
    migrate_legacy_consult()?;
    let store = RuntimeStore::current()?;
    let impact =
        crate::mentions::with_persona_invocation_registry(scope, persona_id, |live_ids| {
            store.read_documents(|snapshot| {
                build_persona_removal_impact_from_snapshot(snapshot, persona_id, live_ids)
            })
        })?;
    let Some(impact) = impact else {
        return Ok(blocked_persona(
            COMMAND,
            "persona_not_found",
            "The Persona is not currently discoverable in the library.",
        ));
    };
    Ok(CommandResult::passed(
        COMMAND,
        "contracted",
        impact,
        Vec::new(),
        Vec::new(),
        false,
        false,
    ))
}

pub fn persona_remove_from_library(
    input: PersonaRemovalCommitInput,
) -> Result<CommandResult<PersonaRemovalOutput>> {
    let scope = OperationScope::for_current_product_host();
    persona_remove_from_library_in_scope(&scope, input)
}

pub fn persona_remove_from_library_in_scope(
    scope: &OperationScope,
    input: PersonaRemovalCommitInput,
) -> Result<CommandResult<PersonaRemovalOutput>> {
    persona_remove_from_library_inner_in_scope(scope, input, false)
}

#[cfg(test)]
fn persona_remove_from_library_inner(
    input: PersonaRemovalCommitInput,
    inject_failure_after_mutations: bool,
) -> Result<CommandResult<PersonaRemovalOutput>> {
    let scope = OperationScope::for_current_product_host();
    persona_remove_from_library_inner_in_scope(&scope, input, inject_failure_after_mutations)
}

fn persona_remove_from_library_inner_in_scope(
    scope: &OperationScope,
    input: PersonaRemovalCommitInput,
    inject_failure_after_mutations: bool,
) -> Result<CommandResult<PersonaRemovalOutput>> {
    const COMMAND: &str = "mom_llama.persona_remove_from_library";
    if input.persona_id.trim().is_empty()
        || input.persona_version == 0
        || !is_sha256(&input.impact_sha256)
    {
        return Ok(CommandResult::blocked(
            COMMAND,
            "stub_blocked",
            Blocker::new(
                "persona_removal_identity_invalid",
                "Remove from Library requires an exact Persona ID, version, and impact SHA-256.",
                vec!["Preview the current removal impact again.".to_string()],
            ),
        ));
    }
    migrate_legacy_consult()?;
    let store = RuntimeStore::current()?;
    crate::mentions::with_persona_invocation_registry(scope, &input.persona_id, |live_ids| {
        // The registry lock is held across the authoritative transaction. A
        // frozen invocation that registered first may finish; once this commit
        // wins, later registrations see the tombstone and fail closed.
        // The cache-authority guard is held through durable removal and both
        // live-tier invalidations. A promotion that wins first is subsequently
        // evicted; one that arrives later observes the tombstone and cannot
        // acquire an owner-generation lease.
        let _cache_authority = acquire_persona_cache_authority_guard(&store)?;
        let attempt = store.mutate_documents(
            PERSONA_REMOVALS_NAMESPACE,
            PersonaRemovalLedger::default,
            |ledger, documents| {
                if let Some(record) = ledger.removals.iter().find(|record| {
                    record.persona_id == input.persona_id
                        && record.persona_version == Some(input.persona_version)
                        && record.impact_sha256 == input.impact_sha256
                }) {
                    let impact = record.impact.clone().ok_or_else(|| {
                        anyhow::anyhow!("matching Persona removal ledger entry has no exact impact")
                    })?;
                    return Ok(PersonaRemovalAttempt::AlreadyCommitted {
                        impact,
                        removed_at: record.removed_at.clone(),
                    });
                }

                let Some(impact) = build_persona_removal_impact_from_documents(
                    documents,
                    &input.persona_id,
                    live_ids,
                )? else {
                    return Ok(PersonaRemovalAttempt::Blocked(Blocker::new(
                        "persona_not_found",
                        "The Persona is not currently discoverable in the library.",
                        vec!["Refresh the Persona library.".to_string()],
                    )));
                };
                if impact.persona_version != input.persona_version {
                    return Ok(PersonaRemovalAttempt::Blocked(Blocker::new(
                        "persona_removal_version_changed",
                        format!(
                            "The Persona is now version {}; the confirmed preview was version {}.",
                            impact.persona_version, input.persona_version
                        ),
                        vec!["Preview the current removal impact again.".to_string()],
                    )));
                }
                if impact.impact_sha256 != input.impact_sha256 {
                    return Ok(PersonaRemovalAttempt::Blocked(Blocker::new(
                        "persona_removal_impact_changed",
                        "The groups, draft, attachments, cache, or invocation impact changed after preview.",
                        vec!["Review and confirm the updated removal impact.".to_string()],
                    )));
                }

                let mut conversations = documents
                    .get::<ConversationDb>(CONVERSATIONS_NAMESPACE)?
                    .unwrap_or_default();
                let Some(index) = conversations.conversations.iter().position(|conversation| {
                    conversation.id == input.persona_id
                        && conversation.kind == ConversationKind::PersonaTemplate
                }) else {
                    anyhow::bail!("Persona disappeared inside its removal transaction");
                };
                let persona_snapshot = conversations.conversations.remove(index);
                if conversations.selected_conversation_id.as_deref()
                    == Some(input.persona_id.as_str())
                {
                    conversations.selected_conversation_id = conversations
                        .conversations
                        .iter()
                        .find(|conversation| conversation.kind == ConversationKind::Chat)
                        .map(|conversation| conversation.id.clone());
                }

                let mut drafts = documents
                    .get::<DraftDb>(DRAFTS_NAMESPACE)?
                    .unwrap_or_default();
                drafts.drafts.retain(|draft| {
                    draft.conversation_id.as_deref() != Some(input.persona_id.as_str())
                });
                let mut groups = documents
                    .get::<PersonaGroupDb>(GROUPS_NAMESPACE)?
                    .unwrap_or_default();
                for group in &mut groups.groups {
                    group.persona_ids.retain(|id| id != &input.persona_id);
                }
                if let Some(provenance) = groups
                    .builtin_persona_provenance
                    .iter_mut()
                    .find(|provenance| provenance.persona_id == input.persona_id)
                {
                    provenance.ownership = BuiltinPersonaOwnership::UserModified;
                    provenance.observed_persona_sha256 = "deleted".to_string();
                }

                let removed_attachments =
                    crate::attachments::remove_persona_draft_attachments_from_documents(
                        documents,
                        &impact
                            .attachments
                            .removed_draft_only_unshared_attachment_ids,
                    )?;
                if removed_attachments
                    != impact
                        .attachments
                        .removed_draft_only_unshared_attachment_ids
                {
                    anyhow::bail!("Persona draft attachment removal changed inside transaction");
                }
                let removed_mom_cache =
                    crate::kv_cache::remove_persona_cache_from_documents(
                        documents,
                        &input.persona_id,
                    )?;
                let removed_native_cache =
                    crate::native_runtime::remove_persona_native_cache_from_documents(
                        documents,
                        &input.persona_id,
                    )?;
                if removed_mom_cache != impact.caches.mom_persistent_cache_ids
                    || removed_native_cache != impact.caches.native_persistent_cache_ids
                {
                    anyhow::bail!("Persona cache impact changed inside removal transaction");
                }

                documents.put_bytes(
                    CONVERSATIONS_NAMESPACE,
                    &serde_json::to_vec(&conversations)?,
                )?;
                documents.put_bytes(DRAFTS_NAMESPACE, &serde_json::to_vec(&drafts)?)?;
                documents.put_bytes(GROUPS_NAMESPACE, &serde_json::to_vec(&groups)?)?;
                let removed_at = now_ms().to_string();
                ledger
                    .removals
                    .retain(|record| record.persona_id != input.persona_id);
                ledger.removals.push(PersonaRemovalRecord {
                    persona_id: input.persona_id.clone(),
                    persona_version: Some(input.persona_version),
                    impact_sha256: input.impact_sha256.clone(),
                    impact: Some(impact.clone()),
                    removed_at: removed_at.clone(),
                    persona_snapshot: Some(persona_snapshot),
                    migration_tombstone: false,
                });
                if inject_failure_after_mutations {
                    anyhow::bail!("injected Persona removal transaction failure");
                }
                Ok(PersonaRemovalAttempt::Committed { impact, removed_at })
            },
        )?;

        let (impact, removed_at, already_removed) = match attempt {
            PersonaRemovalAttempt::Blocked(blocker) => {
                return Ok(CommandResult::blocked(COMMAND, "stub_blocked", blocker));
            }
            PersonaRemovalAttempt::Committed { impact, removed_at } => (impact, removed_at, false),
            PersonaRemovalAttempt::AlreadyCommitted { impact, removed_at } => {
                (impact, removed_at, true)
            }
        };
        let post_memory = crate::kv_cache::invalidate_persona_memory_cache(&input.persona_id)?;
        let post_native =
            crate::native_runtime::invalidate_loaded_native_cache_owner(&input.persona_id)?;
        Ok(CommandResult::passed(
            COMMAND,
            "contracted",
            PersonaRemovalOutput {
                impact,
                removed_at,
                already_removed,
                evicted_memory_cache_entries: post_memory,
                evicted_native_live_cache_entries: post_native,
            },
            vec![store.path().display().to_string()],
            Vec::new(),
            false,
            false,
        ))
    })
}

fn build_persona_removal_impact_from_snapshot(
    snapshot: &DocumentSnapshot<'_, '_, '_>,
    persona_id: &str,
    live_invocation_ids: &[String],
) -> Result<Option<PersonaRemovalImpact>> {
    let conversations = snapshot
        .get::<ConversationDb>(CONVERSATIONS_NAMESPACE)?
        .unwrap_or_default();
    let Some(persona) = conversations.conversations.iter().find(|conversation| {
        conversation.id == persona_id && conversation.kind == ConversationKind::PersonaTemplate
    }) else {
        return Ok(None);
    };
    let drafts = snapshot
        .get::<DraftDb>(DRAFTS_NAMESPACE)?
        .unwrap_or_default();
    let groups = snapshot
        .get::<PersonaGroupDb>(GROUPS_NAMESPACE)?
        .unwrap_or_default();
    let versions = snapshot
        .get::<PersonaVersionDb>(PERSONA_VERSIONS_NAMESPACE)?
        .unwrap_or_default();
    let attachment_impact = crate::attachments::persona_attachment_impact_from_snapshot(
        snapshot,
        persona,
        &conversations,
        &drafts,
    )?;
    let invocation_impact = crate::mentions::persona_invocation_impact_from_snapshot(
        snapshot,
        persona_id,
        live_invocation_ids,
    )?;
    build_persona_removal_impact(
        persona,
        &drafts,
        &groups,
        &versions,
        attachment_impact,
        crate::kv_cache::persona_cache_ids_from_snapshot(snapshot, persona_id)?,
        crate::native_runtime::persona_native_cache_ids_from_snapshot(snapshot, persona_id)?,
        invocation_impact,
    )
    .map(Some)
}

fn build_persona_removal_impact_from_documents(
    documents: &DocumentMutations<'_, '_, '_>,
    persona_id: &str,
    live_invocation_ids: &[String],
) -> Result<Option<PersonaRemovalImpact>> {
    let conversations = documents
        .get::<ConversationDb>(CONVERSATIONS_NAMESPACE)?
        .unwrap_or_default();
    let Some(persona) = conversations.conversations.iter().find(|conversation| {
        conversation.id == persona_id && conversation.kind == ConversationKind::PersonaTemplate
    }) else {
        return Ok(None);
    };
    let drafts = documents
        .get::<DraftDb>(DRAFTS_NAMESPACE)?
        .unwrap_or_default();
    let groups = documents
        .get::<PersonaGroupDb>(GROUPS_NAMESPACE)?
        .unwrap_or_default();
    let versions = documents
        .get::<PersonaVersionDb>(PERSONA_VERSIONS_NAMESPACE)?
        .unwrap_or_default();
    let attachment_impact = crate::attachments::persona_attachment_impact_from_documents(
        documents,
        persona,
        &conversations,
        &drafts,
    )?;
    let invocation_impact = crate::mentions::persona_invocation_impact_from_documents(
        documents,
        persona_id,
        live_invocation_ids,
    )?;
    build_persona_removal_impact(
        persona,
        &drafts,
        &groups,
        &versions,
        attachment_impact,
        crate::kv_cache::persona_cache_ids_from_documents(documents, persona_id)?,
        crate::native_runtime::persona_native_cache_ids_from_documents(documents, persona_id)?,
        invocation_impact,
    )
    .map(Some)
}

#[allow(clippy::too_many_arguments)]
fn build_persona_removal_impact(
    persona: &Conversation,
    drafts: &DraftDb,
    groups: &PersonaGroupDb,
    versions: &PersonaVersionDb,
    attachment_impact: crate::attachments::PersonaAttachmentRemovalImpact,
    mut mom_persistent_cache_ids: Vec<String>,
    mut native_persistent_cache_ids: Vec<String>,
    invocation_impact: crate::mentions::PersonaInvocationRemovalImpact,
) -> Result<PersonaRemovalImpact> {
    let mut group_impact = groups
        .groups
        .iter()
        .flat_map(|group| {
            group
                .persona_ids
                .iter()
                .enumerate()
                .filter(|(_, id)| id.as_str() == persona.id.as_str())
                .map(|(member_index, _)| PersonaRemovalGroupImpact {
                    group_id: group.id.clone(),
                    group_name: group.name.clone(),
                    member_index,
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    group_impact.sort_by(|left, right| {
        (&left.group_id, left.member_index).cmp(&(&right.group_id, right.member_index))
    });
    let mut draft_impact = drafts
        .drafts
        .iter()
        .filter(|draft| draft.conversation_id.as_deref() == Some(persona.id.as_str()))
        .map(|draft| {
            let mut attachment_ids = draft.attachment_ids.clone();
            attachment_ids.sort();
            Ok(PersonaRemovalDraftImpact {
                updated_at: draft.updated_at.clone(),
                message_sha256: sha256_json(&draft.message)?,
                attachment_ids,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    draft_impact.sort_by(|left, right| {
        (&left.updated_at, &left.message_sha256, &left.attachment_ids).cmp(&(
            &right.updated_at,
            &right.message_sha256,
            &right.attachment_ids,
        ))
    });
    let mut retained_message_ids = persona
        .messages
        .iter()
        .map(|message| message.id.clone())
        .collect::<Vec<_>>();
    retained_message_ids.sort();
    let mut retained_persona_versions = versions
        .versions
        .iter()
        .filter(|version| version.persona_id == persona.id.as_str())
        .map(|version| version.version)
        .collect::<Vec<_>>();
    retained_persona_versions.sort_unstable();
    retained_persona_versions.dedup();
    mom_persistent_cache_ids.sort();
    native_persistent_cache_ids.sort();

    let mut impact = PersonaRemovalImpact {
        schema: PERSONA_REMOVAL_SCHEMA.to_string(),
        persona_id: persona.id.clone(),
        persona_version: persona.execution_profile.version,
        persona_title: persona.title.clone(),
        persona_snapshot_sha256: sha256_json(persona)?,
        groups: group_impact,
        drafts: draft_impact,
        attachments: PersonaRemovalAttachmentImpact {
            draft_attachment_ids: attachment_impact.draft_attachment_ids,
            removed_draft_only_unshared_attachment_ids: attachment_impact
                .draft_only_unshared_attachment_ids,
            retained_supporting_attachment_ids: attachment_impact
                .retained_supporting_attachment_ids,
        },
        caches: PersonaRemovalCacheImpact {
            owner_id: persona.id.clone(),
            mom_persistent_cache_ids,
            native_persistent_cache_ids,
        },
        active_invocation_ids: invocation_impact.active_invocation_ids,
        retained_history: PersonaRemovalHistoryImpact {
            retained_message_ids,
            retained_persona_versions,
            retained_invocation_ids: invocation_impact.retained_invocation_ids,
        },
        impact_sha256: String::new(),
    };
    impact.impact_sha256 = sha256_json(&impact)?;
    Ok(impact)
}

pub(crate) fn acquire_persona_cache_authority_guard(
    store: &RuntimeStore,
) -> Result<PersonaCacheAuthorityGuard> {
    let process_guard = PERSONA_CACHE_AUTHORITY
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| anyhow::anyhow!("Persona cache authority lock is unavailable"))?;
    let lock_path = store
        .path()
        .with_file_name(PERSONA_CACHE_AUTHORITY_LOCK_FILE);
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    let file = options.open(lock_path)?;
    FileExt::lock_exclusive(&file)?;
    Ok(PersonaCacheAuthorityGuard {
        _process_guard: process_guard,
        file,
    })
}

pub(crate) fn acquire_persona_cache_owner_lease(
    store: &RuntimeStore,
    owner_id: &str,
) -> Result<Option<PersonaCacheOwnerLease>> {
    let authority = acquire_persona_cache_authority_guard(store)?;
    let removal_generation = persona_cache_owner_removal_generation(store, owner_id)?;
    if removal_generation != 0 {
        return Ok(None);
    }
    Ok(Some(PersonaCacheOwnerLease {
        _authority: authority,
        store: store.clone(),
        owner_id: owner_id.to_string(),
        removal_generation,
    }))
}

fn persona_cache_owner_removal_generation(store: &RuntimeStore, owner_id: &str) -> Result<u64> {
    let ledger = store
        .get::<PersonaRemovalLedger>(PERSONA_REMOVALS_NAMESPACE)?
        .unwrap_or_default();
    Ok(ledger
        .removals
        .iter()
        .filter(|record| record.persona_id == owner_id)
        .count() as u64)
}

pub(crate) fn persona_cache_owner_is_removed_from_documents(
    documents: &DocumentMutations<'_, '_, '_>,
    owner_id: &str,
) -> Result<bool> {
    let ledger = documents
        .get::<PersonaRemovalLedger>(PERSONA_REMOVALS_NAMESPACE)?
        .unwrap_or_default();
    Ok(persona_is_tombstoned(&ledger, owner_id))
}

pub(crate) fn persona_ids_are_removed(persona_ids: &[String]) -> Result<Vec<String>> {
    let ledger = RuntimeStore::current()?
        .get::<PersonaRemovalLedger>(PERSONA_REMOVALS_NAMESPACE)?
        .unwrap_or_default();
    let mut removed = persona_ids
        .iter()
        .filter(|id| persona_is_tombstoned(&ledger, id))
        .cloned()
        .collect::<Vec<_>>();
    removed.sort();
    removed.dedup();
    Ok(removed)
}

pub(crate) fn reject_removed_conversation_writes_from_documents(
    conversations: &ConversationDb,
    documents: &DocumentMutations<'_, '_, '_>,
) -> Result<()> {
    let ledger = documents
        .get::<PersonaRemovalLedger>(PERSONA_REMOVALS_NAMESPACE)?
        .unwrap_or_default();
    let mut removed_ids = conversations
        .conversations
        .iter()
        .filter(|conversation| persona_is_tombstoned(&ledger, &conversation.id))
        .map(|conversation| conversation.id.clone())
        .collect::<Vec<_>>();
    removed_ids.sort();
    removed_ids.dedup();
    if !removed_ids.is_empty() {
        anyhow::bail!(
            "refusing stale conversation write for removed Persona owner(s): {}",
            removed_ids.join(", ")
        );
    }
    Ok(())
}

pub(crate) fn reject_removed_conversation_id_from_documents(
    conversation_id: &str,
    documents: &DocumentMutations<'_, '_, '_>,
) -> Result<()> {
    let ledger = documents
        .get::<PersonaRemovalLedger>(PERSONA_REMOVALS_NAMESPACE)?
        .unwrap_or_default();
    if persona_is_tombstoned(&ledger, conversation_id) {
        anyhow::bail!(
            "refusing stale conversation upsert for removed Persona owner {conversation_id}"
        );
    }
    Ok(())
}

pub(crate) fn filter_removed_persona_drafts_from_documents(
    drafts: &mut DraftDb,
    documents: &DocumentMutations<'_, '_, '_>,
) -> Result<Vec<String>> {
    let ledger = documents
        .get::<PersonaRemovalLedger>(PERSONA_REMOVALS_NAMESPACE)?
        .unwrap_or_default();
    let mut filtered = drafts
        .drafts
        .iter()
        .filter_map(|draft| draft.conversation_id.as_deref())
        .filter(|conversation_id| persona_is_tombstoned(&ledger, conversation_id))
        .map(str::to_string)
        .collect::<Vec<_>>();
    drafts.drafts.retain(|draft| {
        !draft
            .conversation_id
            .as_deref()
            .is_some_and(|conversation_id| persona_is_tombstoned(&ledger, conversation_id))
    });
    filtered.sort();
    filtered.dedup();
    Ok(filtered)
}

fn persona_is_tombstoned(ledger: &PersonaRemovalLedger, persona_id: &str) -> bool {
    ledger
        .removals
        .iter()
        .any(|record| record.persona_id == persona_id)
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub fn persona_instantiate(
    persona_id: &str,
    title: Option<String>,
) -> Result<CommandResult<Conversation>> {
    persona_instantiate_inner(persona_id, title, || {})
}

fn persona_instantiate_inner(
    persona_id: &str,
    title: Option<String>,
    before_admission: impl FnOnce(),
) -> Result<CommandResult<Conversation>> {
    migrate_legacy_consult()?;
    let store = RuntimeStore::current()?;
    let _attachments = crate::attachments::lock_attachment_lifecycle()?;
    let _ = crate::attachments::load_attachment_db()?;
    let id = Uuid::new_v4().to_string();
    let now = now_ms().to_string();
    before_admission();
    let admitted = store.mutate_documents(
        CONVERSATIONS_NAMESPACE,
        ConversationDb::default,
        |db, documents| {
            if persona_cache_owner_is_removed_from_documents(documents, persona_id)? {
                return Ok(Err(Blocker::new(
                    "persona_not_found",
                    "The Persona is no longer discoverable in the library.",
                    vec!["Refresh Personas in Settings.".to_string()],
                )));
            }
            reject_removed_conversation_writes_from_documents(db, documents)?;
            let Some(persona) = db
                .conversations
                .iter()
                .find(|conversation| {
                    conversation.id == persona_id
                        && conversation.kind == ConversationKind::PersonaTemplate
                })
                .cloned()
            else {
                return Ok(Err(Blocker::new(
                    "persona_not_found",
                    "The Persona no longer exists.",
                    vec!["Refresh Personas in Settings.".to_string()],
                )));
            };
            let current_version = build_persona_version(&persona)?;
            let versions = documents
                .get::<PersonaVersionDb>(PERSONA_VERSIONS_NAMESPACE)?
                .unwrap_or_default();
            if !versions.versions.iter().any(|version| {
                version.persona_id == current_version.persona_id
                    && version.version == current_version.version
                    && version.profile_sha256 == current_version.profile_sha256
                    && version.conversation_sha256 == current_version.conversation_sha256
            }) {
                return Ok(Err(Blocker::new(
                    "persona_version_unavailable",
                    "The current Persona version has no exact immutable version record.",
                    vec![
                        "Refresh or re-save the Persona before starting a conversation."
                            .to_string(),
                    ],
                )));
            }
            let mut profile = persona.execution_profile.clone();
            profile.tool_bindings = match normalize_tools(profile.tool_bindings) {
                Ok(tools) => tools,
                Err(blocker) => return Ok(Err(blocker)),
            };
            let groups = documents
                .get::<PersonaGroupDb>(GROUPS_NAMESPACE)?
                .unwrap_or_default();
            profile.mention_handle =
                unique_handle(db, &groups.groups, &format!("{}-chat", persona.title));
            profile.version = 1;
            let mut messages = remap_messages(&id, active_path_messages(&persona));
            crate::attachments::snapshot_message_attachments_from_documents(
                &id,
                &mut messages,
                documents,
            )?;
            let conversation = Conversation {
                id: id.clone(),
                title: title
                    .clone()
                    .unwrap_or_else(|| format!("Chat with {}", persona.title)),
                created_at: now.clone(),
                updated_at: now.clone(),
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
            db.selected_conversation_id = Some(id.clone());
            db.conversations.insert(0, conversation.clone());
            Ok(Ok(conversation))
        },
    )?;
    let conversation = match admitted {
        Ok(conversation) => conversation,
        Err(blocker) => {
            return Ok(CommandResult::blocked(
                "mom_llama.persona_instantiate",
                "stub_blocked",
                blocker,
            ));
        }
    };
    Ok(CommandResult::passed(
        "mom_llama.persona_instantiate",
        "contracted",
        conversation,
        vec![store.path().display().to_string()],
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
    migrate_legacy_consult()?;
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
    let db = load_db()?;
    Ok((db.conversations, load_group_db()?.groups))
}

fn write_group(
    group_id: Option<String>,
    name: String,
    mention_handle: String,
    persona_ids: Vec<String>,
    command: &str,
) -> Result<CommandResult<PersonaGroup>> {
    migrate_legacy_consult()?;
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
    if unique.len() != persona_ids.len() {
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
    let id = group_id.unwrap_or_else(|| Uuid::new_v4().to_string());
    let store = RuntimeStore::current()?;
    let written = store.mutate_documents(
        GROUPS_NAMESPACE,
        PersonaGroupDb::default,
        |groups, documents| {
            let conversations = documents
                .get::<ConversationDb>(CONVERSATIONS_NAMESPACE)?
                .unwrap_or_default();
            if persona_ids.iter().any(|persona_id| {
                !conversations.conversations.iter().any(|conversation| {
                    conversation.id == *persona_id
                        && conversation.kind == ConversationKind::PersonaTemplate
                })
            }) {
                return Ok(Err(Blocker::new(
                    "persona_group_member_invalid",
                    "Every consult-group member must be a distinct discoverable Persona.",
                    vec!["Refresh Personas and choose valid members.".to_string()],
                )));
            }
            let handle = match validate_available_handle(
                &conversations,
                None,
                Some(&id),
                &mention_handle,
                &groups.groups,
            ) {
                Ok(handle) => handle,
                Err(blocker) => return Ok(Err(blocker)),
            };
            let now = now_ms().to_string();
            let created_at = groups
                .groups
                .iter()
                .find(|group| group.id == id)
                .map(|group| group.created_at.clone())
                .unwrap_or_else(|| now.clone());
            let group = PersonaGroup {
                id: id.clone(),
                name: clean_name(&name, "Consult group"),
                mention_handle: handle,
                persona_ids: persona_ids.clone(),
                created_at,
                updated_at: now,
            };
            groups.groups.retain(|candidate| candidate.id != id);
            groups.groups.insert(0, group.clone());
            Ok(Ok(group))
        },
    )?;
    let group = match written {
        Ok(group) => group,
        Err(blocker) => return Ok(CommandResult::blocked(command, "stub_blocked", blocker)),
    };
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
    if groups.legacy_consult_migration_is_current() {
        return Ok(());
    }

    let mut conversations = load_db()?;
    repair_legacy_handles(&mut conversations, &mut groups);

    let settings = resolve_settings()?;
    let catalog = builtin_personas();
    let catalog_versions = reconcile_builtin_personas(
        &mut conversations,
        &mut groups,
        catalog,
        LIBRARY_REVISION,
        settings.model_path.clone(),
        settings.mmproj_path.clone(),
    )?;

    // Migration reads the persisted legacy document directly. The display
    // list intentionally overlays current built-ins and cannot be a lossless
    // recovery source when a stored user panel collides with a built-in ID.
    let current_builtin_panels = builtin_panels();
    let mut migrated_legacy_persona_ids = BTreeSet::new();
    for panel in stored_legacy_panels()? {
        if current_builtin_panels
            .iter()
            .any(|built_in| built_in == &panel)
        {
            continue;
        }
        validate_legacy_panel(&panel)?;
        let mut persona_ids = Vec::new();
        let mut distinct_persona_ids = BTreeSet::new();
        for (index, legacy) in panel.personas.iter().enumerate() {
            let id = resolve_legacy_persona_id(&conversations, &panel, index, legacy)?;
            if !distinct_persona_ids.insert(id.clone()) {
                anyhow::bail!(
                    "legacy Consult panel `{}` contains duplicate Persona content",
                    panel.id
                );
            }
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
                        system_message: Some(legacy.perspective_prompt.clone()),
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
            migrated_legacy_persona_ids.insert(id.clone());
            persona_ids.push(id);
        }
        let group_id = resolve_legacy_group_id(
            &groups.groups,
            &panel,
            &persona_ids,
            &current_builtin_panels,
        )?;
        if !groups.groups.iter().any(|group| group.id == group_id) {
            let now = now_ms().to_string();
            let handle = unique_handle(&conversations, &groups.groups, &panel.name);
            groups.groups.push(PersonaGroup {
                id: group_id,
                name: panel.name.clone(),
                mention_handle: handle,
                persona_ids,
                created_at: now.clone(),
                updated_at: now,
            });
        }
    }
    repair_legacy_handles(&mut conversations, &mut groups);
    let dangling_persona_ids = repair_dangling_group_members(&conversations, &mut groups);
    groups.legacy_consult_migrated = true;
    groups.migration_version = PERSONA_STATE_MIGRATION_VERSION;
    groups.catalog_revision = Some(LIBRARY_REVISION.to_string());
    save_db(&conversations)?;
    let legacy_versions = conversations
        .conversations
        .iter()
        .filter(|persona| migrated_legacy_persona_ids.contains(&persona.id))
        .cloned()
        .collect::<Vec<_>>();
    let mut recorded_version_ids = BTreeSet::new();
    for persona in catalog_versions.into_iter().chain(legacy_versions) {
        if !recorded_version_ids.insert(persona.id.clone()) {
            continue;
        }
        record_persona_version(&persona)?;
    }
    persist_persona_removal_migration(&conversations, &groups, &dangling_persona_ids)?;
    Ok(())
}

fn repair_dangling_group_members(
    conversations: &ConversationDb,
    groups: &mut PersonaGroupDb,
) -> Vec<String> {
    let valid_persona_ids = conversations
        .conversations
        .iter()
        .filter(|conversation| conversation.kind == ConversationKind::PersonaTemplate)
        .map(|conversation| conversation.id.as_str())
        .collect::<BTreeSet<_>>();
    let mut removed_persona_ids = Vec::new();
    for group in &mut groups.groups {
        let before = group.persona_ids.len();
        let mut retained = Vec::with_capacity(group.persona_ids.len());
        for id in group.persona_ids.drain(..) {
            if valid_persona_ids.contains(id.as_str()) {
                retained.push(id);
            } else {
                removed_persona_ids.push(id);
            }
        }
        if retained.len() != before {
            group.updated_at = now_ms().to_string();
        }
        group.persona_ids = retained;
    }
    groups.groups.retain(|group| !group.persona_ids.is_empty());
    removed_persona_ids.sort();
    removed_persona_ids.dedup();
    removed_persona_ids
}

fn persist_persona_removal_migration(
    conversations: &ConversationDb,
    groups: &PersonaGroupDb,
    dangling_persona_ids: &[String],
) -> Result<()> {
    let present = conversations
        .conversations
        .iter()
        .filter(|conversation| conversation.kind == ConversationKind::PersonaTemplate)
        .map(|conversation| conversation.id.as_str())
        .collect::<BTreeSet<_>>();
    let mut absent_persona_ids = groups
        .builtin_persona_provenance
        .iter()
        .filter(|provenance| {
            provenance.observed_persona_sha256 == "deleted"
                && !present.contains(provenance.persona_id.as_str())
        })
        .map(|provenance| provenance.persona_id.clone())
        .chain(dangling_persona_ids.iter().cloned())
        .collect::<Vec<_>>();
    absent_persona_ids.sort();
    absent_persona_ids.dedup();
    let store = RuntimeStore::current()?;
    let _cache_authority = acquire_persona_cache_authority_guard(&store)?;
    store.mutate_documents(
        PERSONA_REMOVALS_NAMESPACE,
        PersonaRemovalLedger::default,
        |ledger, documents| {
            for persona_id in &absent_persona_ids {
                if !persona_is_tombstoned(ledger, persona_id) {
                    let removed_at = now_ms().to_string();
                    ledger.removals.push(PersonaRemovalRecord {
                        persona_id: persona_id.clone(),
                        persona_version: None,
                        impact_sha256: sha256_json(&("persona_removal_migration.v1", persona_id))?,
                        impact: None,
                        removed_at,
                        persona_snapshot: None,
                        migration_tombstone: true,
                    });
                }
                crate::kv_cache::remove_persona_cache_from_documents(documents, persona_id)?;
                crate::native_runtime::remove_persona_native_cache_from_documents(
                    documents, persona_id,
                )?;
            }
            documents.put_bytes(GROUPS_NAMESPACE, &serde_json::to_vec(groups)?)?;
            Ok(())
        },
    )?;
    for persona_id in absent_persona_ids {
        crate::kv_cache::invalidate_persona_memory_cache(&persona_id)?;
        crate::native_runtime::invalidate_loaded_native_cache_owner(&persona_id)?;
    }
    Ok(())
}

fn validate_legacy_panel(panel: &ConsultPanel) -> Result<()> {
    if panel.name.trim().is_empty() {
        anyhow::bail!("legacy Consult panel `{}` has an empty name", panel.id);
    }
    if panel.personas.is_empty() || panel.personas.len() > MAX_GROUP_MEMBERS {
        anyhow::bail!(
            "legacy Consult panel `{}` must contain one to four Personas",
            panel.id
        );
    }
    if panel.personas.iter().any(|persona| {
        persona.label.trim().is_empty() || persona.perspective_prompt.trim().is_empty()
    }) {
        anyhow::bail!(
            "legacy Consult panel `{}` contains an incomplete Persona",
            panel.id
        );
    }
    Ok(())
}

fn resolve_legacy_persona_id(
    conversations: &ConversationDb,
    panel: &ConsultPanel,
    index: usize,
    legacy: &ConsultPersona,
) -> Result<String> {
    let legacy_id = legacy.id.trim();
    if !legacy_id.is_empty() {
        let candidate = format!("persona-{legacy_id}");
        match conversations
            .conversations
            .iter()
            .find(|conversation| conversation.id == candidate)
        {
            None => return Ok(candidate),
            Some(conversation) if legacy_persona_matches(conversation, legacy) => {
                return Ok(candidate);
            }
            Some(_) => {}
        }
    }

    let digest = sha256_json(&(panel.id.as_str(), index, legacy))?;
    let candidate = format!("persona-legacy-{digest}");
    match conversations
        .conversations
        .iter()
        .find(|conversation| conversation.id == candidate)
    {
        None => Ok(candidate),
        Some(conversation) if legacy_persona_matches(conversation, legacy) => Ok(candidate),
        Some(_) => anyhow::bail!(
            "legacy Persona identity collision for panel `{}` seat {}",
            panel.id,
            index + 1
        ),
    }
}

fn legacy_persona_matches(conversation: &Conversation, legacy: &ConsultPersona) -> bool {
    conversation.kind == ConversationKind::PersonaTemplate
        && conversation.title == legacy.label
        && conversation.execution_profile.system_message.as_deref()
            == Some(legacy.perspective_prompt.as_str())
}

fn resolve_legacy_group_id(
    groups: &[PersonaGroup],
    panel: &ConsultPanel,
    persona_ids: &[String],
    current_builtin_panels: &[ConsultPanel],
) -> Result<String> {
    let panel_id = panel.id.trim();
    let reserved_builtin_id = panel_id.starts_with("builtin-")
        || current_builtin_panels
            .iter()
            .any(|built_in| built_in.id == panel_id);
    if !panel_id.is_empty() && !reserved_builtin_id {
        let candidate = format!("group-{panel_id}");
        match groups.iter().find(|group| group.id == candidate) {
            None => return Ok(candidate),
            Some(group) if legacy_group_matches(group, panel, persona_ids) => return Ok(candidate),
            Some(_) => {}
        }
    }

    let digest = sha256_json(panel)?;
    let candidate = format!("group-legacy-{digest}");
    match groups.iter().find(|group| group.id == candidate) {
        None => Ok(candidate),
        Some(group) if legacy_group_matches(group, panel, persona_ids) => Ok(candidate),
        Some(_) => anyhow::bail!("legacy Consult group identity collision for `{}`", panel.id),
    }
}

fn legacy_group_matches(
    group: &PersonaGroup,
    panel: &ConsultPanel,
    persona_ids: &[String],
) -> bool {
    group.name == panel.name && group.persona_ids == persona_ids
}

fn reconcile_builtin_personas(
    conversations: &mut ConversationDb,
    groups: &mut PersonaGroupDb,
    catalog: Vec<ConsultPersona>,
    catalog_revision: &str,
    default_model_path: Option<PathBuf>,
    default_mmproj_path: Option<PathBuf>,
) -> Result<Vec<Conversation>> {
    let mut versioned_ids = BTreeSet::new();
    for source in catalog {
        let id = format!("persona-{}", source.id);
        let catalog_entry_sha256 = sha256_json(&source)?;
        let prior = groups
            .builtin_persona_provenance
            .iter()
            .find(|provenance| provenance.persona_id == id)
            .cloned();

        if let Some(index) = conversations
            .conversations
            .iter()
            .position(|conversation| conversation.id == id)
        {
            let current_sha256 =
                builtin_persona_content_sha256(&conversations.conversations[index])?;
            let ownership = match prior.as_ref() {
                Some(provenance)
                    if provenance.ownership == BuiltinPersonaOwnership::CatalogManaged
                        && provenance.last_applied_persona_sha256.as_deref()
                            == Some(current_sha256.as_str()) =>
                {
                    BuiltinPersonaOwnership::CatalogManaged
                }
                Some(_) => BuiltinPersonaOwnership::UserModified,
                None if is_pristine_legacy_builtin(
                    &conversations.conversations[index],
                    &source,
                ) =>
                {
                    BuiltinPersonaOwnership::CatalogManaged
                }
                None => BuiltinPersonaOwnership::UserModified,
            };

            let (last_applied_persona_sha256, observed_persona_sha256) =
                if ownership == BuiltinPersonaOwnership::CatalogManaged {
                    let persona = &mut conversations.conversations[index];
                    let changed = persona.title != source.label
                        || persona.kind != ConversationKind::PersonaTemplate
                        || persona.execution_profile.system_message.as_deref()
                            != Some(source.perspective_prompt.as_str());
                    if changed {
                        persona.title = source.label.clone();
                        persona.kind = ConversationKind::PersonaTemplate;
                        persona.execution_profile.system_message =
                            Some(source.perspective_prompt.clone());
                        persona.execution_profile.version =
                            persona.execution_profile.version.saturating_add(1);
                        persona.updated_at = now_ms().to_string();
                        versioned_ids.insert(id.clone());
                    }
                    let applied = builtin_persona_content_sha256(persona)?;
                    (Some(applied.clone()), applied)
                } else {
                    (
                        prior.and_then(|provenance| provenance.last_applied_persona_sha256),
                        current_sha256,
                    )
                };
            replace_builtin_provenance(
                groups,
                BuiltinPersonaProvenance {
                    persona_id: id,
                    ownership,
                    catalog_revision: catalog_revision.to_string(),
                    catalog_entry_sha256,
                    last_applied_persona_sha256,
                    observed_persona_sha256,
                },
            );
            continue;
        }

        // Once a catalog Persona has been observed, its later absence is a
        // user deletion, not a seeding failure. Preserve that tombstone across
        // catalog revisions instead of silently resurrecting the template.
        if let Some(prior) = prior {
            replace_builtin_provenance(
                groups,
                BuiltinPersonaProvenance {
                    persona_id: id,
                    ownership: BuiltinPersonaOwnership::UserModified,
                    catalog_revision: catalog_revision.to_string(),
                    catalog_entry_sha256,
                    last_applied_persona_sha256: prior.last_applied_persona_sha256,
                    observed_persona_sha256: "deleted".to_string(),
                },
            );
            continue;
        }

        let desired_handle = source.id.replace('_', "-");
        let handle = if conversations.conversations.iter().any(|conversation| {
            normalize_handle(&conversation.execution_profile.mention_handle) == desired_handle
        }) {
            unique_handle(conversations, &groups.groups, &source.label)
        } else {
            desired_handle
        };
        let now = now_ms().to_string();
        let persona = Conversation {
            id: id.clone(),
            title: source.label,
            created_at: now.clone(),
            updated_at: now,
            kind: ConversationKind::PersonaTemplate,
            execution_profile: ConversationExecutionProfile {
                mention_handle: handle,
                model_path: default_model_path.clone(),
                mmproj_path: default_mmproj_path.clone(),
                system_message: Some(source.perspective_prompt),
                ..ConversationExecutionProfile::default()
            },
            selected_model_path: default_model_path.clone(),
            source_conversation_id: None,
            source_message_id: None,
            branch_root_message_id: None,
            active_leaf_message_id: None,
            current_skill_ids: Vec::new(),
            messages: Vec::new(),
        };
        let applied_persona_sha256 = builtin_persona_content_sha256(&persona)?;
        replace_builtin_provenance(
            groups,
            BuiltinPersonaProvenance {
                persona_id: id.clone(),
                ownership: BuiltinPersonaOwnership::CatalogManaged,
                catalog_revision: catalog_revision.to_string(),
                catalog_entry_sha256,
                last_applied_persona_sha256: Some(applied_persona_sha256.clone()),
                observed_persona_sha256: applied_persona_sha256,
            },
        );
        versioned_ids.insert(id);
        conversations.conversations.push(persona);
    }

    Ok(conversations
        .conversations
        .iter()
        .filter(|conversation| versioned_ids.contains(&conversation.id))
        .cloned()
        .collect())
}

fn replace_builtin_provenance(groups: &mut PersonaGroupDb, provenance: BuiltinPersonaProvenance) {
    groups
        .builtin_persona_provenance
        .retain(|candidate| candidate.persona_id != provenance.persona_id);
    groups.builtin_persona_provenance.push(provenance);
}

fn builtin_persona_content_sha256(persona: &Conversation) -> Result<String> {
    sha256_json(&(
        &persona.id,
        &persona.title,
        &persona.kind,
        &persona.execution_profile,
        &persona.selected_model_path,
        &persona.source_conversation_id,
        &persona.source_message_id,
        &persona.branch_root_message_id,
        &persona.active_leaf_message_id,
        &persona.current_skill_ids,
        &persona.messages,
    ))
}

fn is_pristine_legacy_builtin(persona: &Conversation, source: &ConsultPersona) -> bool {
    persona.kind == ConversationKind::PersonaTemplate
        && persona.title == source.label
        && normalize_handle(&persona.execution_profile.mention_handle)
            == source.id.replace('_', "-")
        && persona.execution_profile.system_message.as_deref()
            == Some(source.perspective_prompt.as_str())
        && persona.execution_profile.sampling.is_none()
        && persona.execution_profile.chat_template == ChatTemplatePolicy::ModelDefault
        && persona.execution_profile.tool_bindings.is_empty()
        && persona.execution_profile.source_history_tokens == 4096
        && persona.execution_profile.host_context_tokens == 2048
        && persona.execution_profile.version == 1
        && persona.selected_model_path == persona.execution_profile.model_path
        && persona.source_conversation_id.is_none()
        && persona.source_message_id.is_none()
        && persona.branch_root_message_id.is_none()
        && persona.active_leaf_message_id.is_none()
        && persona.current_skill_ids.is_empty()
        && persona.messages.is_empty()
}

fn load_group_db() -> Result<PersonaGroupDb> {
    Ok(RuntimeStore::current()?
        .get(GROUPS_NAMESPACE)?
        .unwrap_or_default())
}

#[derive(Debug, Clone, Copy)]
enum LegacyHandleOwner {
    Conversation(usize),
    Group(usize),
}

fn repair_legacy_handles(db: &mut ConversationDb, groups: &mut PersonaGroupDb) -> bool {
    let mut owners = db
        .conversations
        .iter()
        .enumerate()
        .map(|(index, conversation)| {
            (
                conversation.created_at.clone(),
                0_u8,
                conversation.id.clone(),
                LegacyHandleOwner::Conversation(index),
            )
        })
        .chain(groups.groups.iter().enumerate().map(|(index, group)| {
            (
                group.created_at.clone(),
                1_u8,
                group.id.clone(),
                LegacyHandleOwner::Group(index),
            )
        }))
        .collect::<Vec<_>>();
    owners.sort_by(|left, right| (&left.0, left.1, &left.2).cmp(&(&right.0, right.1, &right.2)));

    let mut used = BTreeSet::new();
    let mut changed = false;
    for (_, _, _, owner) in owners {
        let (stored, label) = match owner {
            LegacyHandleOwner::Conversation(index) => (
                db.conversations[index]
                    .execution_profile
                    .mention_handle
                    .clone(),
                db.conversations[index].title.clone(),
            ),
            LegacyHandleOwner::Group(index) => (
                groups.groups[index].mention_handle.clone(),
                groups.groups[index].name.clone(),
            ),
        };
        let normalized = normalize_handle(&stored);
        let base = if valid_normalized_handle(&normalized) {
            normalized
        } else {
            slug(&label)
        };
        let migrated = unique_from_used(&mut used, &base);
        if stored != migrated {
            match owner {
                LegacyHandleOwner::Conversation(index) => {
                    db.conversations[index].execution_profile.mention_handle = migrated;
                }
                LegacyHandleOwner::Group(index) => {
                    groups.groups[index].mention_handle = migrated;
                }
            }
            changed = true;
        }
    }

    for conversation in &mut db.conversations {
        if conversation.execution_profile.model_path.is_none()
            && conversation.selected_model_path.is_some()
        {
            conversation.execution_profile.model_path = conversation.selected_model_path.clone();
            changed = true;
        }
    }
    changed
}

fn validate_available_handle(
    conversations: &ConversationDb,
    current_conversation_id: Option<&str>,
    current_group_id: Option<&str>,
    value: &str,
    groups: &[PersonaGroup],
) -> Result<String, Blocker> {
    let handle = normalize_handle(value);
    if !valid_normalized_handle(&handle) {
        return Err(Blocker::new(
            "mention_handle_invalid",
            "Handles use 2–48 lowercase letters, numbers, or hyphens.",
            vec!["Choose a handle such as `evidence-lens`.".to_string()],
        ));
    }
    let conversation_taken = conversations.conversations.iter().any(|conversation| {
        Some(conversation.id.as_str()) != current_conversation_id
            && normalize_handle(&conversation.execution_profile.mention_handle) == handle
    });
    let group_taken = groups.iter().any(|group| {
        Some(group.id.as_str()) != current_group_id
            && normalize_handle(&group.mention_handle) == handle
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

pub(crate) fn validate_imported_conversation_handle(
    conversations: &ConversationDb,
    value: &str,
    title: &str,
) -> Result<std::result::Result<String, Blocker>> {
    let groups = load_group_db()?;
    if value.trim().is_empty() {
        return Ok(Ok(unique_handle(conversations, &groups.groups, title)));
    }
    Ok(validate_available_handle(
        conversations,
        None,
        None,
        value,
        &groups.groups,
    ))
}

fn valid_normalized_handle(handle: &str) -> bool {
    (2..=48).contains(&handle.len())
        && handle.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
        })
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
    let base = if valid_normalized_handle(base) {
        base
    } else {
        "chat"
    };
    let mut candidate = base.to_string();
    let mut suffix = 2;
    while used.contains(&candidate) {
        let suffix_text = format!("-{suffix}");
        let keep = 48usize.saturating_sub(suffix_text.len());
        candidate = format!(
            "{}{}",
            base.chars().take(keep).collect::<String>(),
            suffix_text
        );
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

fn normalize_tools(tools: Vec<ToolBinding>) -> std::result::Result<Vec<ToolBinding>, Blocker> {
    if tools.len() > MAX_PERSONA_TOOL_BINDINGS {
        return Err(Blocker::new(
            "persona_tool_binding_limit_exceeded",
            format!("A Persona may attach at most {MAX_PERSONA_TOOL_BINDINGS} tools."),
            vec![format!(
                "Remove tool bindings until no more than {MAX_PERSONA_TOOL_BINDINGS} remain."
            )],
        ));
    }
    let mut seen = BTreeSet::new();
    Ok(tools
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
        .collect())
}

fn clean_name(value: &str, fallback: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        fallback.to_string()
    } else {
        value.chars().take(96).collect()
    }
}

fn blocked_persona<T>(command: &str, code: &str, message: &str) -> CommandResult<T>
where
    T: Serialize,
{
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
    use super::{
        BuiltinPersonaOwnership, GROUPS_NAMESPACE, MAX_PERSONA_TOOL_BINDINGS,
        PERSONA_REMOVALS_NAMESPACE, PERSONA_STATE_MIGRATION_VERSION, PERSONA_VERSIONS_NAMESPACE,
        PersonaGroupDb, PersonaRemovalCommitInput, PersonaRemovalLedger, PersonaVersion,
        PersonaVersionDb, builtin_persona_content_sha256, normalize_handle, normalize_tools,
        persona_instantiate, persona_instantiate_inner, persona_removal_preview,
        persona_remove_from_library, persona_remove_from_library_inner, reconcile_builtin_personas,
        repair_dangling_group_members, repair_legacy_handles, slug, validate_available_handle,
    };
    use crate::config::{lock_data_dir_override_for_tests, set_data_dir_override_for_tests};
    use crate::consult::ConsultPersona;
    use crate::conversation_store::{
        CONVERSATIONS_NAMESPACE, Conversation, ConversationDb, ConversationExecutionProfile,
        ConversationKind, DRAFTS_NAMESPACE, DraftDb, DraftMessage, Message, MessageRole,
        ToolBinding, load_db, save_db,
    };
    use crate::persona_library::LIBRARY_REVISION;
    use crate::store::RuntimeStore;
    use std::collections::BTreeSet;
    use std::path::PathBuf;
    use std::sync::{Arc, Barrier, MutexGuard};
    use std::thread;
    use uuid::Uuid;

    struct TestDataDir {
        _guard: MutexGuard<'static, ()>,
        path: PathBuf,
    }

    impl TestDataDir {
        fn new(label: &str) -> Self {
            let guard = lock_data_dir_override_for_tests();
            let path = std::env::temp_dir().join(format!(
                "mom-llama-persona-removal-{label}-{}",
                Uuid::new_v4().simple()
            ));
            std::fs::create_dir_all(&path).expect("create Persona removal test data dir");
            set_data_dir_override_for_tests(Some(path.clone()));
            Self {
                _guard: guard,
                path,
            }
        }
    }

    impl Drop for TestDataDir {
        fn drop(&mut self) {
            set_data_dir_override_for_tests(None);
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn removal_persona(id: &str) -> Conversation {
        Conversation {
            id: id.to_string(),
            title: "Removal fixture".to_string(),
            created_at: "1".to_string(),
            updated_at: "2".to_string(),
            kind: ConversationKind::PersonaTemplate,
            execution_profile: ConversationExecutionProfile {
                mention_handle: "removal-fixture".to_string(),
                version: 7,
                ..ConversationExecutionProfile::default()
            },
            selected_model_path: None,
            source_conversation_id: Some("source-chat".to_string()),
            source_message_id: Some("source-message".to_string()),
            branch_root_message_id: Some("retained-message".to_string()),
            active_leaf_message_id: Some("retained-message".to_string()),
            current_skill_ids: Vec::new(),
            messages: vec![Message {
                id: "retained-message".to_string(),
                conversation_id: id.to_string(),
                role: MessageRole::User,
                content: "Retain this frozen history.".to_string(),
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
            }],
        }
    }

    fn seed_removal_fixture() -> (RuntimeStore, Conversation, ConversationDb) {
        let store = RuntimeStore::current().expect("open removal store");
        let persona = removal_persona(&Uuid::new_v4().to_string());
        let conversations = ConversationDb {
            conversations: vec![persona.clone()],
            selected_conversation_id: Some(persona.id.clone()),
        };
        let groups = PersonaGroupDb {
            groups: vec![super::PersonaGroup {
                id: "fixture-group".to_string(),
                name: "Fixture group".to_string(),
                mention_handle: "fixture-group".to_string(),
                persona_ids: vec![persona.id.clone()],
                created_at: "1".to_string(),
                updated_at: "1".to_string(),
            }],
            legacy_consult_migrated: true,
            migration_version: PERSONA_STATE_MIGRATION_VERSION,
            catalog_revision: Some(LIBRARY_REVISION.to_string()),
            builtin_persona_provenance: Vec::new(),
        };
        let drafts = DraftDb {
            drafts: vec![DraftMessage {
                conversation_id: Some(persona.id.clone()),
                message: "Draft that must be removed".to_string(),
                attachment_ids: Vec::new(),
                updated_at: "3".to_string(),
            }],
        };
        let versions = PersonaVersionDb {
            versions: vec![PersonaVersion {
                persona_id: persona.id.clone(),
                version: 7,
                profile_sha256: "profile".to_string(),
                conversation_sha256: "conversation".to_string(),
                created_at: "2".to_string(),
            }],
        };
        store
            .put(CONVERSATIONS_NAMESPACE, &conversations)
            .expect("seed conversations");
        store.put(GROUPS_NAMESPACE, &groups).expect("seed groups");
        store.put(DRAFTS_NAMESPACE, &drafts).expect("seed draft");
        store
            .put(PERSONA_VERSIONS_NAMESPACE, &versions)
            .expect("seed Persona versions");
        (store, persona, conversations)
    }

    fn tool_binding(server: &str, tool: &str) -> ToolBinding {
        ToolBinding {
            server: server.to_string(),
            tool: tool.to_string(),
        }
    }

    fn legacy_conversation(id: &str, title: &str, created_at: &str, handle: &str) -> Conversation {
        Conversation {
            id: id.to_string(),
            title: title.to_string(),
            created_at: created_at.to_string(),
            updated_at: created_at.to_string(),
            kind: ConversationKind::Chat,
            execution_profile: ConversationExecutionProfile {
                mention_handle: handle.to_string(),
                ..ConversationExecutionProfile::default()
            },
            selected_model_path: None,
            source_conversation_id: None,
            source_message_id: None,
            branch_root_message_id: None,
            active_leaf_message_id: None,
            current_skill_ids: Vec::new(),
            messages: Vec::new(),
        }
    }

    fn catalog_persona(label: &str, prompt: &str) -> ConsultPersona {
        ConsultPersona {
            id: "careful_guide".to_string(),
            label: label.to_string(),
            description: "A deterministic catalog fixture.".to_string(),
            perspective_prompt: prompt.to_string(),
            public_figure: None,
            expertise: Some("Testing".to_string()),
            model_slot: None,
        }
    }

    fn seed(
        conversations: &mut ConversationDb,
        groups: &mut PersonaGroupDb,
        revision: &str,
        label: &str,
        prompt: &str,
    ) -> Vec<crate::conversation_store::Conversation> {
        reconcile_builtin_personas(
            conversations,
            groups,
            vec![catalog_persona(label, prompt)],
            revision,
            None,
            None,
        )
        .expect("catalog reconciliation must succeed")
    }

    #[test]
    fn mention_handles_are_stable_and_human_readable() {
        assert_eq!(normalize_handle("@Evidence-Lens"), "evidence-lens");
        assert_eq!(slug("  Whole-person lens  "), "whole-person-lens");
    }

    #[test]
    fn persona_tool_normalization_preserves_a_bounded_stable_set() {
        let mut bindings = (0..MAX_PERSONA_TOOL_BINDINGS)
            .map(|index| tool_binding(" local-server ", &format!(" tool-{index} ")))
            .collect::<Vec<_>>();
        bindings[1] = tool_binding(" local-server ", " tool-0 ");

        let normalized = normalize_tools(bindings).expect("bounded bindings must normalize");

        assert_eq!(normalized.len(), MAX_PERSONA_TOOL_BINDINGS - 1);
        assert_eq!(normalized[0], tool_binding("local-server", "tool-0"));
        assert_eq!(normalized[1], tool_binding("local-server", "tool-2"));
    }

    #[test]
    fn persona_tool_normalization_rejects_instead_of_truncating_over_limit() {
        let bindings = (0..=MAX_PERSONA_TOOL_BINDINGS)
            .map(|index| tool_binding("local-server", &format!("tool-{index}")))
            .collect::<Vec<_>>();

        let blocker = normalize_tools(bindings).expect_err("over-limit bindings must be rejected");

        assert_eq!(blocker.code, "persona_tool_binding_limit_exceeded");
        assert!(
            blocker
                .message
                .contains(&MAX_PERSONA_TOOL_BINDINGS.to_string())
        );
    }

    #[test]
    fn legacy_handle_collisions_are_repaired_in_stable_owner_order() {
        let mut conversations = ConversationDb {
            conversations: vec![
                legacy_conversation("newer", "Newer owner", "2", "SHARED"),
                legacy_conversation("older", "Older owner", "1", "shared"),
                legacy_conversation("invalid", "Careful Guide", "3", "not valid!"),
            ],
            selected_conversation_id: Some("newer".to_string()),
        };
        let mut groups = PersonaGroupDb {
            groups: vec![super::PersonaGroup {
                id: "group".to_string(),
                name: "Shared group".to_string(),
                mention_handle: "Shared".to_string(),
                persona_ids: Vec::new(),
                created_at: "4".to_string(),
                updated_at: "4".to_string(),
            }],
            ..PersonaGroupDb::default()
        };

        assert!(repair_legacy_handles(&mut conversations, &mut groups));
        let handle = |id: &str| {
            conversations
                .conversations
                .iter()
                .find(|conversation| conversation.id == id)
                .expect("legacy conversation")
                .execution_profile
                .mention_handle
                .clone()
        };
        assert_eq!(handle("older"), "shared");
        assert_eq!(handle("newer"), "shared-2");
        assert_eq!(handle("invalid"), "careful-guide");
        assert_eq!(groups.groups[0].mention_handle, "shared-3");
        let after = (conversations.clone(), groups.clone());
        assert!(!repair_legacy_handles(&mut conversations, &mut groups));
        assert_eq!((conversations, groups), after);
    }

    #[test]
    fn same_id_in_another_namespace_never_exempts_a_handle_collision() {
        let conversations = ConversationDb {
            conversations: vec![legacy_conversation(
                "shared-id",
                "Conversation",
                "1",
                "occupied",
            )],
            selected_conversation_id: None,
        };
        let groups = vec![super::PersonaGroup {
            id: "shared-id".to_string(),
            name: "Group".to_string(),
            mention_handle: "group-handle".to_string(),
            persona_ids: Vec::new(),
            created_at: "1".to_string(),
            updated_at: "1".to_string(),
        }];
        assert_eq!(
            validate_available_handle(
                &conversations,
                None,
                Some("shared-id"),
                "occupied",
                &groups,
            )
            .expect_err("a group update cannot borrow a conversation handle")
            .code,
            "mention_handle_taken"
        );
        assert_eq!(
            validate_available_handle(
                &conversations,
                Some("shared-id"),
                None,
                "group-handle",
                &groups,
            )
            .expect_err("a conversation update cannot borrow a group handle")
            .code,
            "mention_handle_taken"
        );
    }

    #[test]
    fn first_catalog_seed_records_managed_provenance() {
        let mut conversations = ConversationDb::default();
        let mut groups = PersonaGroupDb::default();

        let versions = seed(
            &mut conversations,
            &mut groups,
            "catalog-v1",
            "Careful guide",
            "Be careful.",
        );

        assert_eq!(versions.len(), 1);
        assert_eq!(conversations.conversations.len(), 1);
        let persona = &conversations.conversations[0];
        let provenance = &groups.builtin_persona_provenance[0];
        assert_eq!(
            provenance.ownership,
            BuiltinPersonaOwnership::CatalogManaged
        );
        assert_eq!(provenance.catalog_revision, "catalog-v1");
        assert_eq!(
            provenance.last_applied_persona_sha256.as_deref(),
            Some(
                builtin_persona_content_sha256(persona)
                    .expect("hash Persona")
                    .as_str()
            )
        );
    }

    #[test]
    fn repeating_the_same_catalog_seed_is_a_noop() {
        let mut conversations = ConversationDb::default();
        let mut groups = PersonaGroupDb::default();
        seed(
            &mut conversations,
            &mut groups,
            "catalog-v1",
            "Careful guide",
            "Be careful.",
        );
        let conversations_before = conversations.clone();
        let groups_before = groups.clone();

        let versions = seed(
            &mut conversations,
            &mut groups,
            "catalog-v1",
            "Careful guide",
            "Be careful.",
        );

        assert!(versions.is_empty());
        assert_eq!(conversations, conversations_before);
        assert_eq!(groups, groups_before);
    }

    #[test]
    fn pristine_catalog_persona_upgrades_safely() {
        let mut conversations = ConversationDb::default();
        let mut groups = PersonaGroupDb::default();
        seed(
            &mut conversations,
            &mut groups,
            "catalog-v1",
            "Careful guide",
            "Be careful.",
        );

        let versions = seed(
            &mut conversations,
            &mut groups,
            "catalog-v2",
            "Careful guide revised",
            "Be careful and concrete.",
        );

        assert_eq!(versions.len(), 1);
        let persona = &conversations.conversations[0];
        assert_eq!(persona.title, "Careful guide revised");
        assert_eq!(
            persona.execution_profile.system_message.as_deref(),
            Some("Be careful and concrete.")
        );
        assert_eq!(persona.execution_profile.version, 2);
        let provenance = &groups.builtin_persona_provenance[0];
        assert_eq!(
            provenance.ownership,
            BuiltinPersonaOwnership::CatalogManaged
        );
        assert_eq!(provenance.catalog_revision, "catalog-v2");
    }

    #[test]
    fn edited_catalog_persona_is_never_overwritten_by_an_upgrade() {
        let mut conversations = ConversationDb::default();
        let mut groups = PersonaGroupDb::default();
        seed(
            &mut conversations,
            &mut groups,
            "catalog-v1",
            "Careful guide",
            "Be careful.",
        );
        let persona = &mut conversations.conversations[0];
        persona.title = "My trusted guide".to_string();
        persona.execution_profile.system_message = Some("Use my own framing.".to_string());
        persona.execution_profile.version = 2;
        persona.messages.push(Message {
            id: "kept-history".to_string(),
            conversation_id: persona.id.clone(),
            role: MessageRole::User,
            content: "This history belongs to the user.".to_string(),
            created_at: "2".to_string(),
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
        });
        persona.active_leaf_message_id = Some("kept-history".to_string());
        let edited = persona.clone();

        let versions = seed(
            &mut conversations,
            &mut groups,
            "catalog-v2",
            "Catalog replacement",
            "Replace the prompt.",
        );

        assert!(versions.is_empty());
        assert_eq!(conversations.conversations[0], edited);
        let provenance = &groups.builtin_persona_provenance[0];
        assert_eq!(provenance.ownership, BuiltinPersonaOwnership::UserModified);
        assert_eq!(provenance.catalog_revision, "catalog-v2");
        assert_eq!(
            provenance.observed_persona_sha256,
            builtin_persona_content_sha256(&edited).expect("hash edited Persona")
        );
    }

    #[test]
    fn deleted_catalog_persona_is_not_silently_reseeded() {
        let mut conversations = ConversationDb::default();
        let mut groups = PersonaGroupDb::default();
        seed(
            &mut conversations,
            &mut groups,
            "catalog-v1",
            "Careful guide",
            "Be careful.",
        );
        conversations.conversations.clear();

        let versions = seed(
            &mut conversations,
            &mut groups,
            "catalog-v2",
            "Careful guide revised",
            "Be careful and concrete.",
        );

        assert!(versions.is_empty());
        assert!(conversations.conversations.is_empty());
        let provenance = &groups.builtin_persona_provenance[0];
        assert_eq!(provenance.ownership, BuiltinPersonaOwnership::UserModified);
        assert_eq!(provenance.observed_persona_sha256, "deleted");
    }

    #[test]
    fn removal_commit_is_exact_idempotent_and_preserves_history() {
        let _session = TestDataDir::new("commit");
        let (store, persona, stale_conversations) = seed_removal_fixture();
        let stale_drafts = store
            .get::<DraftDb>(DRAFTS_NAMESPACE)
            .expect("read stale drafts")
            .expect("stale draft document");
        let impact = persona_removal_preview(&persona.id)
            .expect("preview removal")
            .result
            .expect("removal impact");
        let cache_lease = super::acquire_persona_cache_owner_lease(&store, &persona.id)
            .expect("acquire active cache-owner lease")
            .expect("active Persona cache owner");
        assert_eq!(cache_lease.owner_generation(), 0);
        assert!(cache_lease.validate().expect("validate active cache owner"));
        drop(cache_lease);
        assert_eq!(impact.persona_version, 7);
        assert_eq!(impact.groups.len(), 1);
        assert_eq!(impact.drafts.len(), 1);
        assert_eq!(
            impact.retained_history.retained_message_ids.as_slice(),
            &["retained-message".to_string()]
        );
        assert_eq!(
            impact.retained_history.retained_persona_versions.as_slice(),
            &[7]
        );
        let input = PersonaRemovalCommitInput {
            persona_id: persona.id.clone(),
            persona_version: impact.persona_version,
            impact_sha256: impact.impact_sha256.clone(),
        };

        let first = persona_remove_from_library(input.clone())
            .expect("commit removal")
            .result
            .expect("removal output");
        assert!(!first.already_removed);
        assert!(
            load_db()
                .expect("load conversations")
                .conversations
                .iter()
                .all(|conversation| conversation.id != persona.id)
        );
        assert!(
            store
                .get::<DraftDb>(DRAFTS_NAMESPACE)
                .expect("read drafts")
                .expect("draft document")
                .drafts
                .iter()
                .all(|draft| draft.conversation_id.as_deref() != Some(persona.id.as_str()))
        );
        assert!(
            store
                .get::<PersonaGroupDb>(GROUPS_NAMESPACE)
                .expect("read groups")
                .expect("group document")
                .groups[0]
                .persona_ids
                .is_empty()
        );
        let ledger = store
            .get::<PersonaRemovalLedger>(PERSONA_REMOVALS_NAMESPACE)
            .expect("read removal ledger")
            .expect("removal ledger");
        let record = ledger
            .removals
            .iter()
            .find(|record| record.persona_id == persona.id)
            .expect("removal tombstone");
        assert_eq!(record.persona_snapshot.as_ref(), Some(&persona));
        assert_eq!(
            store
                .get::<PersonaVersionDb>(PERSONA_VERSIONS_NAMESPACE)
                .expect("read versions")
                .expect("version document")
                .versions
                .len(),
            1
        );

        assert!(
            save_db(&stale_conversations).is_err(),
            "a generic stale whole-document writer must fail closed"
        );
        assert!(
            load_db()
                .expect("reload conversations")
                .conversations
                .iter()
                .all(|conversation| conversation.id != persona.id),
            "a generic stale save must not resurrect a removed Persona"
        );
        assert!(
            super::acquire_persona_cache_owner_lease(&store, &persona.id)
                .expect("check removed cache owner")
                .is_none(),
            "the durable removal generation must deny every later cache promotion lease"
        );
        assert!(
            crate::conversation_store::upsert_conversation(
                stale_conversations.clone(),
                persona.clone(),
            )
            .is_err(),
            "a stale per-conversation writer must not reinsert a removed Persona"
        );
        assert!(
            crate::conversation_store::conversation_system_message_update(
                &persona.id,
                Some("stale writer".to_string()),
            )
            .is_err(),
            "conversation instruction updates must not recreate a tombstoned Persona ID"
        );
        let instantiate = persona_instantiate(&persona.id, None)
            .expect("removed Persona instantiation returns a typed blocker");
        assert_eq!(
            instantiate
                .blocker
                .expect("removed Persona instantiate blocker")
                .code,
            "persona_not_found"
        );
        crate::attachments::persist_drafts_with_attachment_gc(&stale_drafts, &BTreeSet::new())
            .expect("attempt stale Persona draft resurrection");
        assert!(
            store
                .get::<DraftDb>(DRAFTS_NAMESPACE)
                .expect("reload drafts")
                .unwrap_or_default()
                .drafts
                .iter()
                .all(|draft| draft.conversation_id.as_deref() != Some(persona.id.as_str())),
            "a generic stale draft save must not resurrect a removed Persona draft"
        );

        let group_update = super::persona_group_update(
            "fixture-group".to_string(),
            "Fixture group".to_string(),
            "fixture-group".to_string(),
            vec![persona.id.clone()],
        )
        .expect("stale group update returns a typed blocker");
        assert_eq!(
            group_update.blocker.expect("stale group blocker").code,
            "persona_group_member_invalid"
        );

        let update = super::persona_update(super::PersonaUpdateInput {
            persona_id: persona.id.clone(),
            name: "Removed Persona".to_string(),
            mention_handle: "removed-persona".to_string(),
            model_path: None,
            mmproj_path: None,
            auto_discover_mmproj: false,
            system_message: None,
            sampling: None,
            chat_template: crate::conversation_store::ChatTemplatePolicy::ModelDefault,
            tool_bindings: Vec::new(),
            source_history_tokens: 4096,
            host_context_tokens: 2048,
        })
        .expect("stale Persona update returns a typed blocker");
        assert_eq!(
            update.blocker.expect("stale Persona update blocker").code,
            "persona_not_found"
        );
        assert_eq!(
            store
                .get::<PersonaVersionDb>(PERSONA_VERSIONS_NAMESPACE)
                .expect("reload versions")
                .expect("version document")
                .versions
                .len(),
            1,
            "a removed Persona cannot acquire a post-removal version"
        );

        let repeated = persona_remove_from_library(input)
            .expect("repeat exact removal")
            .result
            .expect("idempotent removal output");
        assert!(repeated.already_removed);
        assert_eq!(repeated.impact, impact);
    }

    #[test]
    fn instantiate_rechecks_exact_persona_authority_in_its_write_transaction() {
        let _session = TestDataDir::new("instantiate-removal-race");
        let (_, persona, _) = seed_removal_fixture();
        let impact = persona_removal_preview(&persona.id)
            .expect("preview removal")
            .result
            .expect("removal impact");
        let before_admission = Arc::new(Barrier::new(2));
        let release_admission = Arc::new(Barrier::new(2));
        let worker_persona_id = persona.id.clone();
        let worker_before = Arc::clone(&before_admission);
        let worker_release = Arc::clone(&release_admission);
        let worker = thread::spawn(move || {
            persona_instantiate_inner(&worker_persona_id, None, || {
                worker_before.wait();
                worker_release.wait();
            })
        });

        before_admission.wait();
        persona_remove_from_library(PersonaRemovalCommitInput {
            persona_id: persona.id.clone(),
            persona_version: impact.persona_version,
            impact_sha256: impact.impact_sha256,
        })
        .expect("commit removal while instantiation is pre-admission")
        .result
        .expect("removal output");
        release_admission.wait();

        let blocked = worker
            .join()
            .expect("instantiation worker")
            .expect("typed instantiation result");
        assert_eq!(
            blocked
                .blocker
                .expect("removed Persona admission blocker")
                .code,
            "persona_not_found"
        );
        assert!(
            load_db()
                .expect("load conversations")
                .conversations
                .iter()
                .all(|conversation| {
                    conversation.source_conversation_id.as_deref() != Some(persona.id.as_str())
                }),
            "a removal that commits first must not be followed by a derived Chat"
        );
    }

    #[test]
    fn removal_hash_revalidates_every_mutable_impact_fact() {
        let _session = TestDataDir::new("hash-cas");
        let (store, persona, _) = seed_removal_fixture();
        let impact = persona_removal_preview(&persona.id)
            .expect("preview removal")
            .result
            .expect("removal impact");
        store
            .mutate(DRAFTS_NAMESPACE, DraftDb::default, |drafts| {
                drafts.drafts[0].message = "Draft changed after preview".to_string();
                Ok(())
            })
            .expect("change exact impact");

        let blocked = persona_remove_from_library(PersonaRemovalCommitInput {
            persona_id: persona.id.clone(),
            persona_version: impact.persona_version,
            impact_sha256: impact.impact_sha256,
        })
        .expect("stale commit returns typed blocker");
        assert_eq!(
            blocked.blocker.expect("impact blocker").code,
            "persona_removal_impact_changed"
        );
        assert!(
            load_db()
                .expect("load unchanged Persona")
                .conversations
                .iter()
                .any(|conversation| conversation.id == persona.id)
        );
        assert!(
            store
                .get::<PersonaRemovalLedger>(PERSONA_REMOVALS_NAMESPACE)
                .expect("read ledger")
                .unwrap_or_default()
                .removals
                .is_empty()
        );
    }

    #[test]
    fn removal_transaction_rolls_back_every_document_on_fault() {
        let _session = TestDataDir::new("fault-rollback");
        let (store, persona, before_conversations) = seed_removal_fixture();
        let before_groups = store
            .get::<PersonaGroupDb>(GROUPS_NAMESPACE)
            .expect("read groups")
            .expect("groups");
        let before_drafts = store
            .get::<DraftDb>(DRAFTS_NAMESPACE)
            .expect("read drafts")
            .expect("drafts");
        let impact = persona_removal_preview(&persona.id)
            .expect("preview removal")
            .result
            .expect("removal impact");

        assert!(
            persona_remove_from_library_inner(
                PersonaRemovalCommitInput {
                    persona_id: persona.id,
                    persona_version: impact.persona_version,
                    impact_sha256: impact.impact_sha256,
                },
                true,
            )
            .is_err()
        );
        assert_eq!(
            store
                .get::<ConversationDb>(CONVERSATIONS_NAMESPACE)
                .expect("read conversations")
                .expect("conversations"),
            before_conversations
        );
        assert_eq!(
            store
                .get::<PersonaGroupDb>(GROUPS_NAMESPACE)
                .expect("read groups")
                .expect("groups"),
            before_groups
        );
        assert_eq!(
            store
                .get::<DraftDb>(DRAFTS_NAMESPACE)
                .expect("read drafts")
                .expect("drafts"),
            before_drafts
        );
        assert!(
            store
                .get::<PersonaRemovalLedger>(PERSONA_REMOVALS_NAMESPACE)
                .expect("read ledger")
                .is_none()
        );
    }

    #[test]
    fn migration_repairs_dangling_group_persona_ids() {
        let persona = removal_persona("present");
        let conversations = ConversationDb {
            conversations: vec![persona],
            selected_conversation_id: None,
        };
        let mut groups = PersonaGroupDb {
            groups: vec![
                super::PersonaGroup {
                    id: "mixed".to_string(),
                    name: "Mixed".to_string(),
                    mention_handle: "mixed".to_string(),
                    persona_ids: vec!["missing".to_string(), "present".to_string()],
                    created_at: "1".to_string(),
                    updated_at: "1".to_string(),
                },
                super::PersonaGroup {
                    id: "empty-after-repair".to_string(),
                    name: "Missing".to_string(),
                    mention_handle: "missing".to_string(),
                    persona_ids: vec!["missing-two".to_string()],
                    created_at: "1".to_string(),
                    updated_at: "1".to_string(),
                },
            ],
            ..PersonaGroupDb::default()
        };

        assert_eq!(
            repair_dangling_group_members(&conversations, &mut groups),
            vec!["missing".to_string(), "missing-two".to_string()]
        );
        assert_eq!(groups.groups.len(), 1);
        assert_eq!(groups.groups[0].persona_ids, vec!["present".to_string()]);
    }
}
