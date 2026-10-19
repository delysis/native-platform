use crate::config::{KvCachePolicy, Settings, resolve_settings, upstream_setting_string};
use crate::native_runtime::{
    clear_native_prefix_cache, resident_model, resident_model_for_profile_if_loaded,
};
use crate::now_ms;
use crate::receipts::{Blocker, CommandResult};
use crate::skill_store::load_skill_db;
use crate::store::{DocumentMutations, DocumentSnapshot, RuntimeStore};
use anyhow::{Result, anyhow};
pub use llama_native_cache::{CacheEntryState, CacheFingerprint, CacheTier};
use llama_native_cache::{
    CacheMatch, MemoryPrefixCache, PrefixCacheMetadata, PrefixCacheValue,
    longest_compatible_prefix, longest_compatible_prefix_for_owner,
};
use llama_native_engine::NativeModelHandle;
use llama_native_types::{
    BranchRequest, ChatMessage, ChatRole, ChatTemplateChoice, GenerationInput, GenerationRequest,
    GenerationState, NativeErrorCode, PromptForm, PromptTokenPolicy, SamplingConfig,
    SequenceStateBlob, SequenceStateCell, SharedPrefixBatchRequest,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;
use uuid::Uuid;

const KV_CACHE_NAMESPACE: &str = "kv-cache.v3";
const KV_CACHE_BLOB_PREFIX: &str = "kv-cache.v3.blob.";
const DEFAULT_MEMORY_CACHE_BYTES: usize = 512 * 1024 * 1024;
const MAX_PERSISTENT_ENTRY_BYTES: usize = 8 * 1024 * 1024 * 1024;
const DEFAULT_PERSISTENT_CACHE_BYTES: usize = 64 * 1024 * 1024 * 1024;
// SQLite limits individual values; keep encryption scratch bounded as well.
const CACHE_BLOB_CHUNK_BYTES: usize = 16 * 1024 * 1024;
const DEFAULT_PERSISTENT_CACHE_ENTRIES: usize = 64;
const BUILTIN_CACHE_OWNER: &str = "builtin:kind-local-assistant";
const BUILTIN_CACHE_PROMPT: &str =
    "You are a kind, concise local assistant. Answer clearly and acknowledge uncertainty.";

pub type KvCacheMetadata = PrefixCacheMetadata;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoicePersonaPrefixPreparation {
    /// Absent when readiness comes directly from worker-owned KV without a
    /// serialized snapshot. Such a hit must not invent a stored cache entry.
    pub metadata: Option<KvCacheMetadata>,
    pub resident_prefix_tokens: usize,
    pub audio_projector_warmed: bool,
    pub prefill_us: u64,
    pub verification_us: u64,
    pub persistence_us: u64,
    pub promotion_us: u64,
    pub persisted: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct VoiceCachePreparationProgress {
    pub completed: usize,
    pub total: usize,
    pub persona_id: String,
    pub persona: String,
    pub reused: bool,
    pub elapsed_ms: u64,
    pub cache_positions: usize,
    pub state_bytes: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct VoiceCachePreparationSummary {
    pub prepared: usize,
    pub reused: usize,
    pub total: usize,
    pub elapsed_ms: u64,
}

fn is_startup_voice_cache_target(conversation: &crate::conversation_store::Conversation) -> bool {
    use crate::conversation_store::ConversationKind;
    conversation.kind == ConversationKind::PersonaTemplate
        || (conversation.kind == ConversationKind::Chat && !conversation.messages.is_empty())
}

/// Explicit setup work, before admitting foreground generation. Persist every
/// persona prefix and the same bounded history projection used by calls. This
/// is deliberately separate from cheap per-launch resident warming.
pub fn prepare_startup_voice_caches(
    scope: &crate::OperationScope,
    mut on_progress: impl FnMut(VoiceCachePreparationProgress),
) -> Result<VoiceCachePreparationSummary> {
    use crate::conversation_store::ConversationKind;
    let started = Instant::now();
    anyhow::ensure!(!scope.cancellation_requested(), "Voice setup was cancelled");
    let settings = resolve_settings()?;
    if !settings.kv_cache_policy.persists_conversation_checkpoints() {
        anyhow::bail!("Voice setup requires persistent conversation caching");
    }
    // Materialize the same catalog that the UI uses, before taking the plan.
    crate::personas::persona_list()?;
    let db = crate::conversation_store::load_db_with_settings(&settings)?;
    let persona_names = db
        .conversations
        .iter()
        .filter(|conversation| conversation.kind == ConversationKind::PersonaTemplate)
        .map(|conversation| (conversation.id.clone(), conversation.title.clone()))
        .collect::<HashMap<_, _>>();
    let mut targets = db
        .conversations
        .into_iter()
        .filter(is_startup_voice_cache_target)
        .collect::<Vec<_>>();
    // Prepare shared persona prefixes before any history that can reuse them.
    targets.sort_by_key(|conversation| conversation.kind != ConversationKind::PersonaTemplate);
    if targets.len() > DEFAULT_PERSISTENT_CACHE_ENTRIES {
        anyhow::bail!("Voice setup exceeds the persistent cache entry budget");
    }
    let total = targets.len();
    let mut prepared_ids = Vec::new();
    let mut prepared_bytes = 0_usize;
    for (index, mut conversation) in targets.into_iter().enumerate() {
        if scope.has_active_voice_call() || scope.active_operation_count() != 0 {
            anyhow::bail!("Voice cache preparation yielded to foreground work");
        }
        let item_started = Instant::now();
        let persona_id = conversation
            .source_conversation_id
            .clone()
            .unwrap_or_else(|| conversation.id.clone());
        let persona_name = persona_names
            .get(&persona_id)
            .cloned()
            .unwrap_or_else(|| "Conversation".to_string());
        let progress = |completed, positions, bytes| VoiceCachePreparationProgress {
            completed,
            total,
            persona_id: persona_id.clone(),
            persona: persona_name.clone(),
            reused: false,
            elapsed_ms: item_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
            cache_positions: positions,
            state_bytes: bytes,
        };
        on_progress(progress(index, 0, 0));
        anyhow::ensure!(!scope.cancellation_requested(), "Voice setup was cancelled");
        let mut profile = settings.clone();
        crate::chat::apply_conversation_model_pair(
            &mut profile,
            conversation.execution_profile.model_path.clone(),
            conversation.selected_model_path.clone(),
            conversation.execution_profile.mmproj_path.clone(),
        );
        let handle = match profile.model_path.as_deref() {
            Some(path) => crate::native_runtime::resident_model_for_profile(
                scope,
                &profile,
                path,
                profile.mmproj_path.as_deref(),
            ),
            None => resident_model(scope, &profile),
        }
        .map_err(|blocked| anyhow!(blocked.blocker.message))?;
        let (value, authority) = if conversation.kind == ConversationKind::PersonaTemplate {
            let messages = stable_persona_prompt_messages(&profile, &conversation)?;
            (
                create_prefix_value(
                    &handle,
                    CacheTier::PersonaPack,
                    Some(conversation.id.clone()),
                    format!("Voice persona: {}", conversation.title),
                    &messages,
                    ChatTemplateChoice::PreferNonThinking,
                )?,
                None,
            )
        } else {
            let active = crate::document::checked_active_messages(&conversation)?;
            let system = conversation
                .execution_profile
                .system_message
                .clone()
                .unwrap_or_else(|| {
                    upstream_setting_string(&profile, "systemMessage").unwrap_or_default()
                });
            let skill = crate::skill_store::applied_skill_prompt(&conversation.current_skill_ids)?;
            let projection = crate::voice_context::fit_voice_context(
                &handle,
                &mut conversation,
                &active,
                crate::voice_context::VoicePrompt {
                    settings: &profile,
                    system: &system,
                    skill: &skill.prompt,
                    current: "",
                    template: ChatTemplateChoice::PreferNonThinking,
                    completion_tokens: 96,
                    mode: crate::voice_context::VoiceHistoryMode::Greeting,
                },
            )?;
            let authority = SessionConversationAuthority::from_conversation(&conversation)?;
            let revision = handle
                .inspect_resident_conversation(None)
                .map_err(|e| anyhow!(e))?
                .revision;
            // A same-build retry can retain already prepared historical audio.
            // The ordinary engine still checks the exact cells against the
            // projected request; an owner match alone cannot authorize reuse.
            restore_latest_session_checkpoint_if_needed(
                &profile,
                &handle,
                &cache_fingerprint(&handle)?,
                &conversation.id,
                revision,
            )?;
            // Use the same owner ordering and longest-prefix selection as a
            // foreground request, including the newly prepared persona fallback.
            let owner_ids = crate::chat::reusable_prefix_owner_ids(
                &conversation.id,
                conversation.source_conversation_id.as_deref(),
                &conversation.current_skill_ids,
                skill.cache_owner_id.is_some(),
                skill.cache_owner_id.as_deref(),
            );
            let (cached_prefix, _) = crate::chat::longest_cached_prefix_for_owners(
                &handle,
                &projection.messages,
                &owner_ids,
                &profile,
                !projection.attachments.media.is_empty(),
                crate::chat::PrefixLookupMode::Persistent,
                ChatTemplateChoice::PreferNonThinking,
            )?;
            let request_id = format!("voice-setup-{}", Uuid::new_v4());
            // Use normal multimodal generation and its cache/cancellation path.
            // One bounded warm token is discarded; no message, transcript or
            // assistant reply is committed. A future call trims to its exact
            // common prefix, including across the trailing assistant marker.
            let output = handle
                .generate(GenerationRequest {
                    request_id: request_id.clone(),
                    model_id: handle.status().model_id,
                    input: GenerationInput::Chat {
                        messages: projection.messages,
                        template: ChatTemplateChoice::PreferNonThinking,
                    },
                    sampling: SamplingConfig {
                        max_tokens: 1,
                        temperature: 0.0,
                        seed: 1,
                        ..SamplingConfig::default()
                    },
                    media: projection.attachments.media,
                    cached_prefix: cached_prefix.map(|(_, sequence)| sequence),
                })
                .map_err(|e| anyhow!(e))?
                .wait()
                .map_err(|e| anyhow!(e))?;
            if output.is_empty()
                || output
                    .iter()
                    .any(|item| item.state != GenerationState::Completed)
            {
                anyhow::bail!("Voice history preparation did not complete");
            }
            let state = handle
                .snapshot_conversation_for_request_if_fits(&request_id, MAX_PERSISTENT_ENTRY_BYTES)
                .map_err(|e| anyhow!(e))?
                .ok_or_else(|| {
                    anyhow!("Voice history exceeds the persistent cache entry budget")
                })?;
            let mut metadata = PrefixCacheMetadata::new(
                Uuid::new_v4().to_string(),
                CacheTier::SessionPersistent,
                cache_fingerprint(&handle)?,
                state.token_ids.clone(),
                state.bytes.len(),
                now_ms(),
            )
            .with_owner(&conversation.id)
            .with_label(format!("Conversation {}", conversation.id));
            metadata.cells.clone_from(&state.cells);
            (
                PrefixCacheValue {
                    metadata,
                    sequence: state,
                },
                Some(authority),
            )
        };
        if !value.is_valid() || !persistent_entry_fits(value.sequence.bytes.len()) {
            anyhow::bail!("Voice preparation produced an invalid or oversized checkpoint");
        }
        prepared_bytes = prepared_bytes
            .checked_add(value.sequence.bytes.len())
            .filter(|bytes| *bytes <= DEFAULT_PERSISTENT_CACHE_BYTES)
            .ok_or_else(|| anyhow!("Voice setup exceeds the persistent cache byte budget"))?;
        let persisted = match &authority {
            Some(authority) => {
                persist_session_checkpoint_value(scope, &profile, &value, authority)?
            }
            None => persist_value(&profile, &value)?,
        };
        if !persisted {
            anyhow::bail!("Voice context changed during cache preparation");
        }
        prepared_ids.push(value.metadata.id.clone());
        on_progress(VoiceCachePreparationProgress {
            completed: index + 1,
            total,
            persona_id,
            persona: persona_name,
            reused: false,
            elapsed_ms: item_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
            cache_positions: value.sequence.cells.len().max(value.sequence.token_count),
            state_bytes: value.sequence.bytes.len(),
        });
    }
    anyhow::ensure!(!scope.cancellation_requested(), "Voice setup was cancelled");
    let store = RuntimeStore::open(&settings.data_dir)?;
    let caches = store
        .get::<KvCacheDb>(KV_CACHE_NAMESPACE)?
        .unwrap_or_default();
    if prepared_ids.iter().any(|id| {
        !caches
            .entries
            .iter()
            .any(|entry| &entry.id == id && entry.state == CacheEntryState::Ready)
    }) {
        anyhow::bail!("A prepared voice checkpoint was evicted before setup completed");
    }
    // Leave the selected conversation in worker-owned RAM, using the same
    // preparation function as pickup. Other contexts remain on encrypted disk.
    let mut selected_settings = settings;
    if let Some(selected) = crate::engine::selected_voice_context_to_warm(&mut selected_settings)? {
        let prepared = kv_cache_prepare_voice_persona_resident(scope, &selected)?;
        if prepared.result.is_none() {
            anyhow::bail!("The selected conversation could not become resident after setup");
        }
    }
    Ok(VoiceCachePreparationSummary {
        prepared: total,
        reused: 0,
        total,
        elapsed_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
    })
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum KvCacheState {
    Disabled,
    UnsupportedByEngine,
    BlockedMissingModel,
    BlockedMissingCacheDir,
    ConfiguredNotVerified,
    PromptSmokeVerified,
    Saved,
    Restored,
    Invalidated,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
struct KvCacheDb {
    #[serde(default)]
    entries: Vec<PrefixCacheMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KvCacheStatus {
    pub status: KvCacheState,
    pub policy: KvCachePolicy,
    pub cache_dir: String,
    pub entries: Vec<PrefixCacheMetadata>,
    pub memory_entries: usize,
    pub memory_bytes: usize,
    pub memory_capacity_bytes: usize,
    pub persistent_bytes: usize,
    pub persistent_capacity_bytes: usize,
    pub persistent_capacity_entries: usize,
}

struct MemoryCacheState {
    cache: MemoryPrefixCache,
    global_generation: u64,
    owner_generations: HashMap<String, u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MemoryCacheGeneration {
    global: u64,
    owner: u64,
}

impl MemoryCacheState {
    fn new() -> Self {
        Self {
            cache: MemoryPrefixCache::new(DEFAULT_MEMORY_CACHE_BYTES),
            global_generation: 0,
            owner_generations: HashMap::new(),
        }
    }

    fn generation(&self, owner_id: Option<&str>) -> MemoryCacheGeneration {
        MemoryCacheGeneration {
            global: self.global_generation,
            owner: owner_id
                .and_then(|owner_id| self.owner_generations.get(owner_id).copied())
                .unwrap_or_default(),
        }
    }

    fn promote_if_current(
        &mut self,
        value: PrefixCacheValue,
        expected: MemoryCacheGeneration,
    ) -> bool {
        if self.generation(value.metadata.owner_id.as_deref()) != expected {
            return false;
        }
        if !value.is_valid() {
            return false;
        }
        let replaces_existing = self.cache.contains_id(&value.metadata.id);
        if !matches!(
            self.cache.insert(value),
            llama_native_cache::CacheInsertOutcome::Stored {
                memory_resident: true,
                ..
            }
        ) {
            return false;
        }
        if replaces_existing {
            self.global_generation = self.global_generation.saturating_add(1);
        }
        true
    }

    fn invalidate_owner(&mut self, owner_id: &str) -> usize {
        let generation = self
            .owner_generations
            .entry(owner_id.to_string())
            .or_default();
        *generation = generation.saturating_add(1);
        self.cache.invalidate_owner(owner_id).len()
    }

    fn clear(&mut self) {
        self.global_generation = self.global_generation.saturating_add(1);
        self.cache.clear();
    }
}

static MEMORY_CACHE: OnceLock<Mutex<MemoryCacheState>> = OnceLock::new();
pub(crate) fn note_committed_session_request(
    scope: &crate::OperationScope,
    conversation_id: &str,
    request_id: &str,
) {
    if let Err(error) = scope.note_committed_session_request(conversation_id, request_id) {
        eprintln!("Unable to register committed cache request: {error:#}");
    }
}

/// Enqueue maintenance; callers never export or write native state inline.
pub(crate) fn schedule_committed_session_checkpoint(
    scope: &crate::OperationScope,
    conversation_id: &str,
    request_id: &str,
) {
    if let Err(error) = scope.schedule_session_checkpoint(conversation_id, request_id) {
        if !scope.cancellation_requested() {
            eprintln!("Unable to schedule idle checkpoint: {error:#}");
        }
    }
}

fn memory_cache() -> &'static Mutex<MemoryCacheState> {
    MEMORY_CACHE.get_or_init(|| Mutex::new(MemoryCacheState::new()))
}

pub fn kv_cache_status() -> Result<CommandResult<KvCacheStatus>> {
    let settings = resolve_settings()?;
    let db = load_db()?;
    let (memory_entries, memory_bytes, memory_capacity_bytes) = memory_totals();
    let persistent_bytes = db
        .entries
        .iter()
        .filter(|entry| entry.state == CacheEntryState::Ready)
        .fold(0_usize, |total, entry| {
            total.saturating_add(entry.state_bytes)
        });
    let status = if !settings.kv_cache_policy.allows_prefix_reuse() {
        KvCacheState::Disabled
    } else if settings.model_path.is_none() {
        KvCacheState::BlockedMissingModel
    } else if db
        .entries
        .iter()
        .any(|entry| entry.state == CacheEntryState::Ready)
    {
        KvCacheState::Saved
    } else {
        KvCacheState::ConfiguredNotVerified
    };
    Ok(CommandResult::passed(
        "mom_llama.kv_cache_status",
        "contracted",
        KvCacheStatus {
            status,
            policy: settings.kv_cache_policy,
            cache_dir: settings
                .data_dir
                .join("runtime.sqlite3")
                .display()
                .to_string(),
            entries: db.entries,
            memory_entries,
            memory_bytes,
            memory_capacity_bytes,
            persistent_bytes,
            persistent_capacity_bytes: DEFAULT_PERSISTENT_CACHE_BYTES,
            persistent_capacity_entries: DEFAULT_PERSISTENT_CACHE_ENTRIES,
        },
        Vec::new(),
        Vec::new(),
        false,
        false,
    ))
}

pub fn kv_cache_save(
    scope: &crate::OperationScope,
    skill_id: Option<String>,
) -> Result<CommandResult<KvCacheMetadata>> {
    let settings = resolve_settings()?;
    if !settings.kv_cache_policy.allows_prefix_reuse() {
        return Ok(CommandResult::blocked(
            "mom_llama.kv_cache_save",
            "stub_blocked",
            Blocker::new(
                "kv_cache_policy_disabled",
                "Persistent prompt caching is disabled.",
                vec!["Enable prompt-prefix or session KV caching.".to_string()],
            ),
        ));
    }
    let handle = match resident_model(scope, &settings) {
        Ok(handle) => handle,
        Err(blocked) => {
            return Ok(CommandResult::blocked(
                "mom_llama.kv_cache_save",
                &blocked.readiness,
                blocked.blocker,
            ));
        }
    };
    let stable_messages = stable_prompt_messages(&settings, skill_id.as_deref())?;
    let owner_id = skill_id
        .clone()
        .unwrap_or_else(|| BUILTIN_CACHE_OWNER.to_string());
    let label = skill_label(skill_id.as_deref())?;
    let value = create_prefix_value(
        &handle,
        CacheTier::PersonaPack,
        Some(owner_id),
        label,
        &stable_messages,
        ChatTemplateChoice::ModelDefault,
    )?;
    if !verify_restore_equivalence(
        &handle,
        &stable_messages,
        &value.sequence,
        ChatTemplateChoice::ModelDefault,
    )? {
        return Ok(CommandResult::blocked(
            "mom_llama.kv_cache_save",
            "blocked_native_runtime",
            Blocker::new(
                "kv_cache_restore_mismatch",
                "The native cache did not reproduce the uncached deterministic continuation.",
                vec!["Rebuild the cache after checking the selected model.".to_string()],
            ),
        ));
    }
    if !persist_value(&settings, &value)? {
        return Ok(CommandResult::blocked(
            "mom_llama.kv_cache_save",
            "stub_blocked",
            Blocker::new(
                "kv_cache_owner_removed",
                "The selected cache owner was removed from the Persona library.",
                vec!["Choose an available Persona or Skill.".to_string()],
            ),
        ));
    }
    Ok(CommandResult::passed(
        "mom_llama.kv_cache_save",
        "prompt_smoke_verified",
        value.metadata.clone(),
        vec![
            encrypted_blob_uri(&value.metadata.id),
            settings
                .data_dir
                .join("runtime.sqlite3")
                .display()
                .to_string(),
        ],
        Vec::new(),
        true,
        false,
    ))
}

/// Explicitly prepare the system prefix used by live calls and voice notes.
/// The ordinary cache command uses the model-default chat template, which can
/// tokenize differently from the voice path's non-thinking template.
pub fn kv_cache_save_voice_persona(
    scope: &crate::OperationScope,
    persona_id: &str,
) -> Result<CommandResult<VoicePersonaPrefixPreparation>> {
    let operation = "mom_llama.kv_cache_save_voice_persona";
    let settings = resolve_settings()?;
    if !settings.kv_cache_policy.allows_prefix_reuse() {
        return Ok(CommandResult::blocked(
            operation,
            "stub_blocked",
            Blocker::new(
                "kv_cache_policy_disabled",
                "Prompt caching is disabled.",
                Vec::new(),
            ),
        ));
    }
    let Some(persona) = crate::personas::persona_list()?
        .result
        .unwrap_or_default()
        .into_iter()
        .find(|persona| persona.id == persona_id)
    else {
        return Ok(CommandResult::blocked(
            operation,
            "stub_blocked",
            Blocker::new(
                "persona_not_found",
                "The selected persona is unavailable.",
                Vec::new(),
            ),
        ));
    };
    let handle = match resident_model(scope, &settings) {
        Ok(handle) => handle,
        Err(blocked) => {
            return Ok(CommandResult::blocked(
                operation,
                &blocked.readiness,
                blocked.blocker,
            ));
        }
    };
    let messages = stable_persona_prompt_messages(&settings, &persona)?;
    let template = ChatTemplateChoice::PreferNonThinking;
    let started = std::time::Instant::now();
    let value = create_prefix_value(
        &handle,
        CacheTier::PersonaPack,
        Some(persona.id.clone()),
        format!("Voice persona: {}", persona.title),
        &messages,
        template.clone(),
    )?;
    let prefill_us = started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
    let started = std::time::Instant::now();
    if !verify_restore_equivalence(&handle, &messages, &value.sequence, template)? {
        return Ok(CommandResult::blocked(
            operation,
            "blocked_native_runtime",
            Blocker::new(
                "kv_cache_restore_mismatch",
                "The voice prefix failed deterministic restoration.",
                Vec::new(),
            ),
        ));
    }
    let verification_us = started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
    let started = std::time::Instant::now();
    if !persist_value(&settings, &value)? {
        return Ok(CommandResult::blocked(
            operation,
            "stub_blocked",
            Blocker::new(
                "kv_cache_owner_removed",
                "The persona was removed during preparation.",
                Vec::new(),
            ),
        ));
    }
    let persistence_us = started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
    Ok(CommandResult::passed(
        operation,
        "prompt_smoke_verified",
        VoicePersonaPrefixPreparation {
            audio_projector_warmed: false,
            resident_prefix_tokens: value.sequence.token_count,
            metadata: Some(value.metadata.clone()),
            prefill_us,
            verification_us,
            persistence_us,
            promotion_us: 0,
            persisted: true,
        },
        vec![encrypted_blob_uri(&value.metadata.id)],
        Vec::new(),
        true,
        false,
    ))
}

/// Prepare one selected voice persona in the resident prefix LRU. This avoids
/// the large encrypted disk write on the foreground path; durable preparation
/// remains an explicit, separately measured operation.
pub fn kv_cache_prepare_voice_persona_resident(
    scope: &crate::OperationScope,
    persona_id: &str,
) -> Result<CommandResult<VoicePersonaPrefixPreparation>> {
    prepare_voice_context_resident(scope, persona_id, false)
}

/// Startup is opportunistic: once any foreground work has used this worker,
/// only an explicit call preparation may choose a different conversation.
pub(crate) fn prepare_startup_voice_context_resident(
    scope: &crate::OperationScope,
    context_id: &str,
) -> Result<CommandResult<VoicePersonaPrefixPreparation>> {
    prepare_voice_context_resident(scope, context_id, true)
}

fn prepare_voice_context_resident(
    scope: &crate::OperationScope,
    persona_id: &str,
    startup_only: bool,
) -> Result<CommandResult<VoicePersonaPrefixPreparation>> {
    let operation = "mom_llama.kv_cache_prepare_voice_persona_resident";
    let superseded = || {
        CommandResult::blocked(
            operation,
            "preparation_superseded",
            Blocker::new(
                "voice_prewarm_superseded",
                "Newer model activity superseded this preparation.",
                Vec::new(),
            ),
        )
    };
    let mut settings = resolve_settings()?;
    if !settings.kv_cache_policy.allows_prefix_reuse() {
        return Ok(CommandResult::blocked(
            operation,
            "stub_blocked",
            Blocker::new(
                "kv_cache_policy_disabled",
                "Prompt caching is disabled.",
                Vec::new(),
            ),
        ));
    }
    // A materialized call can have a frozen profile different from its source
    // persona. Prepare that actual profile, never overwrite it with the source.
    let conversations = crate::conversation_store::load_db_with_settings(&settings)?;
    let Some(mut persona) = conversations
        .conversations
        .iter()
        .find(|persona| persona.id == persona_id)
        .cloned()
    else {
        return Ok(CommandResult::blocked(
            operation,
            "stub_blocked",
            Blocker::new(
                "persona_not_found",
                "The selected persona is unavailable.",
                Vec::new(),
            ),
        ));
    };
    let session_owner_id = persona.id.clone();
    if let Some(source) = persona.source_conversation_id.as_ref().and_then(|id| {
        conversations
            .conversations
            .iter()
            .find(|item| &item.id == id)
    }) && source.execution_profile.system_message == persona.execution_profile.system_message
        && source.execution_profile.model_path == persona.execution_profile.model_path
        && source.execution_profile.mmproj_path == persona.execution_profile.mmproj_path
        && source.selected_model_path == persona.selected_model_path
        && source.current_skill_ids == persona.current_skill_ids
    {
        // Identical inherited profiles share their prepared persona entry.
        persona.id = source.id.clone();
    }
    crate::chat::apply_conversation_model_pair(
        &mut settings,
        persona.execution_profile.model_path.clone(),
        persona.selected_model_path.clone(),
        persona.execution_profile.mmproj_path.clone(),
    );
    let acquisition = if startup_only {
        // Background preparation must not switch models either. The startup
        // owner already acquired its model; a different selected profile is
        // left to explicit foreground preparation.
        let Some(model_path) = settings.model_path.as_deref() else {
            return Ok(superseded());
        };
        resident_model_for_profile_if_loaded(
            scope,
            &settings,
            model_path,
            settings.mmproj_path.as_deref(),
        )
    } else {
        resident_model(scope, &settings).map(Some)
    };
    let handle = match acquisition {
        Ok(Some(handle)) => handle,
        Ok(None) => return Ok(superseded()),
        Err(blocked) => {
            return Ok(CommandResult::blocked(
                operation,
                &blocked.readiness,
                blocked.blocker,
            ));
        }
    };
    // Call readiness belongs to this exact frozen profile, not a possibly
    // failed or different model from asynchronous startup. Worker-local warm
    // memoization makes repeated call preparation cheap and leaves KV intact.
    let audio_projector_warmed = handle
        .warm_audio_projector()
        .map_err(|error| anyhow!(error))?;
    // A previous hangup may still be encrypting its snapshot. If the worker
    // owns this conversation's newer committed request, do not replace it with
    // an older disk checkpoint (or touch the disk cache at all).
    let pending_request = scope.committed_session_request(&session_owner_id)?;
    let resident = handle
        .inspect_resident_conversation(pending_request.as_deref())
        .map_err(|error| anyhow!(error))?;
    // Comparing only against the revision observed here is insufficient for
    // startup: foreground work may have completed before this inspection. A
    // fresh worker begins at zero; conditional imports/prefills below retain
    // that same zero fence across disk reads and tokenization.
    if startup_only && resident.revision != 0 {
        return Ok(superseded());
    }
    if let Some(count) = resident.committed_prefix_tokens {
        return Ok(CommandResult::passed(
            operation,
            "resident_prefix_prepared",
            VoicePersonaPrefixPreparation {
                audio_projector_warmed,
                metadata: None,
                resident_prefix_tokens: count,
                prefill_us: 0,
                verification_us: 0,
                persistence_us: 0,
                promotion_us: 0,
                persisted: false,
            },
            Vec::new(),
            Vec::new(),
            true,
            false,
        ));
    }
    let fingerprint = cache_fingerprint(&handle)?;
    if let Some(metadata) = restore_latest_session_checkpoint_if_needed(
        &settings,
        &handle,
        &fingerprint,
        &session_owner_id,
        resident.revision,
    )? {
        return Ok(CommandResult::passed(
            operation,
            "resident_prefix_prepared",
            VoicePersonaPrefixPreparation {
                audio_projector_warmed,
                resident_prefix_tokens: metadata.cells.len().max(metadata.token_ids.len()),
                metadata: Some(metadata),
                prefill_us: 0,
                verification_us: 0,
                persistence_us: 0,
                promotion_us: 0,
                persisted: true,
            },
            Vec::new(),
            Vec::new(),
            true,
            false,
        ));
    }
    let messages = stable_persona_prompt_messages(&settings, &persona)?;
    let template = ChatTemplateChoice::PreferNonThinking;
    // First-run setup and the webview can both request launch warming. A
    // matching worker-owned state is already evaluated; exporting it again
    // merely serialized seconds of duplicate work onto call pickup.
    let tokenized = handle
        .tokenize_messages_with_template(messages.clone(), template.clone())
        .map_err(|error| anyhow!(error))?;
    // Probe only tokenization: a live conversation can own the whole profile
    // even when no serialized copy exists in the RAM LRU or on disk.
    let mut probes = Vec::new();
    for text in ["alpha", "beta"] {
        let mut probe = messages.clone();
        probe.push(ChatMessage {
            role: ChatRole::User,
            content: text.to_string(),
        });
        probes.push(
            handle
                .tokenize_messages_with_template(probe, template.clone())
                .map_err(|error| anyhow!(error))?,
        );
    }
    let common =
        llama_native_cache::common_prefix_tokens(&probes[0].token_ids, &probes[1].token_ids);
    if common > 0
        && probes.iter().all(|probe| {
            probe
                .resident_text_prefix_tokens
                .max(probe.resident_media_prefix_tokens)
                >= common
        })
    {
        return Ok(CommandResult::passed(
            operation,
            "resident_prefix_prepared",
            VoicePersonaPrefixPreparation {
                audio_projector_warmed,
                metadata: None,
                resident_prefix_tokens: common,
                prefill_us: 0,
                verification_us: 0,
                persistence_us: 0,
                promotion_us: 0,
                persisted: false,
            },
            Vec::new(),
            Vec::new(),
            true,
            false,
        ));
    }
    let mut live_value = {
        let mut memory = memory_cache()
            .lock()
            .map_err(|_| anyhow!("Persona prefix memory cache is unavailable"))?;
        memory
            .cache
            .best_match_for_owner(&fingerprint, &tokenized.token_ids, &persona.id)
            .and_then(|matched| memory.cache.get(&matched.id, now_ms()))
    };
    if let Some(value) = live_value.as_ref()
        && resident_prefix_covers_snapshot(
            tokenized.resident_text_prefix_tokens,
            tokenized.resident_media_prefix_tokens,
            &tokenized.token_ids,
            &value.sequence.token_ids,
        )
    {
        return Ok(CommandResult::passed(
            operation,
            "resident_prefix_prepared",
            VoicePersonaPrefixPreparation {
                audio_projector_warmed,
                resident_prefix_tokens: value.sequence.token_count,
                metadata: Some(value.metadata.clone()),
                prefill_us: 0,
                verification_us: 0,
                persistence_us: 0,
                promotion_us: 0,
                persisted: false,
            },
            Vec::new(),
            Vec::new(),
            true,
            false,
        ));
    }
    if live_value.is_none() {
        let store = RuntimeStore::open(&settings.data_dir)?;
        let db = store
            .get::<KvCacheDb>(KV_CACHE_NAMESPACE)?
            .unwrap_or_default();
        if let Some(candidate) = longest_compatible_prefix_for_owner(
            &db.entries,
            &fingerprint,
            &tokenized.token_ids,
            &persona.id,
        ) && let Some(metadata) = db.entries.iter().find(|entry| entry.id == candidate.id)
        {
            if resident_prefix_covers_snapshot(
                tokenized.resident_text_prefix_tokens,
                tokenized.resident_media_prefix_tokens,
                &tokenized.token_ids,
                &metadata.token_ids,
            ) {
                return Ok(CommandResult::passed(
                    operation,
                    "resident_prefix_prepared",
                    VoicePersonaPrefixPreparation {
                        audio_projector_warmed,
                        resident_prefix_tokens: metadata.token_ids.len(),
                        metadata: Some(metadata.clone()),
                        prefill_us: 0,
                        verification_us: 0,
                        persistence_us: 0,
                        promotion_us: 0,
                        persisted: false,
                    },
                    Vec::new(),
                    Vec::new(),
                    true,
                    false,
                ));
            }
            live_value = load_persistent_value_or_invalidate(&settings, metadata)?;
        }
    }
    if let Some(value) = live_value.filter(|value| {
        // The shared-prefix prefill intentionally leaves the branch boundary
        // unevaluated. The exact token-prefix match was already established by
        // best_match_for_owner above; requiring equal lengths rejects it.
        handle.has_reusable_sequence(&value.sequence)
            && tokenized.token_ids.starts_with(&value.sequence.token_ids)
    }) {
        if !resident_prefix_covers_snapshot(
            tokenized.resident_text_prefix_tokens,
            tokenized.resident_media_prefix_tokens,
            &tokenized.token_ids,
            &value.sequence.token_ids,
        ) && handle
            .restore_sequence_if_revision(value.sequence.clone(), 0, resident.revision)
            .map_err(|error| anyhow!(error))?
            .is_none()
        {
            return Ok(superseded());
        }
        return Ok(CommandResult::passed(
            operation,
            "resident_prefix_prepared",
            VoicePersonaPrefixPreparation {
                audio_projector_warmed,
                resident_prefix_tokens: value.sequence.token_count,
                metadata: Some(value.metadata),
                prefill_us: 0,
                verification_us: 0,
                persistence_us: 0,
                promotion_us: 0,
                persisted: false,
            },
            Vec::new(),
            Vec::new(),
            true,
            false,
        ));
    }
    let started = std::time::Instant::now();
    let request =
        shared_prefix_preparation_request(&handle, &messages, Some(&persona.id), template)?;
    let Some(token_count) = handle
        .prefill_resident_prefix_if_revision(request, resident.revision)
        .map_err(|error| anyhow!(error))?
    else {
        return Ok(superseded());
    };
    // Readiness means evaluated KV is resident, not that a multi-gigabyte
    // snapshot has been copied, encrypted, and written. The committed-session
    // scheduler persists the complete conversation after the call/idle window.
    // Explicit cache creation continues to export and persist PersonaPacks.
    let prefill_us = started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
    Ok(CommandResult::passed(
        operation,
        "resident_prefix_prepared",
        VoicePersonaPrefixPreparation {
            audio_projector_warmed,
            resident_prefix_tokens: token_count,
            metadata: None,
            prefill_us,
            verification_us: 0,
            persistence_us: 0,
            promotion_us: 0,
            persisted: false,
        },
        Vec::new(),
        Vec::new(),
        true,
        false,
    ))
}

pub fn kv_cache_restore(
    scope: &crate::OperationScope,
    cache_id: Option<String>,
) -> Result<CommandResult<KvCacheMetadata>> {
    let settings = resolve_settings()?;
    if !settings.kv_cache_policy.allows_prefix_reuse() {
        return Ok(CommandResult::blocked(
            "mom_llama.kv_cache_restore",
            "stub_blocked",
            Blocker::new(
                "kv_cache_policy_disabled",
                "Prompt caching is off.",
                vec!["Choose Automatic or Prefixes only in Settings.".to_string()],
            ),
        ));
    }
    let handle = match resident_model(scope, &settings) {
        Ok(handle) => handle,
        Err(blocked) => {
            return Ok(CommandResult::blocked(
                "mom_llama.kv_cache_restore",
                &blocked.readiness,
                blocked.blocker,
            ));
        }
    };
    let db = load_db()?;
    let Some(index) = cache_id
        .as_deref()
        .and_then(|id| db.entries.iter().position(|entry| entry.id == id))
        .or_else(|| {
            db.entries.iter().position(|entry| {
                entry.state == CacheEntryState::Ready && entry.tier == CacheTier::PersonaPack
            })
        })
    else {
        return Ok(CommandResult::blocked(
            "mom_llama.kv_cache_restore",
            "stub_blocked",
            Blocker::new(
                "kv_cache_not_found",
                "No saved native prefix cache was found.",
                vec!["Create a persona or session cache first.".to_string()],
            ),
        ));
    };
    let expected = cache_fingerprint(&handle)?;
    let metadata = db.entries[index].clone();
    if metadata.tier == CacheTier::SessionPersistent {
        return Ok(CommandResult::blocked(
            "mom_llama.kv_cache_restore",
            "stub_blocked",
            Blocker::new(
                "session_cache_requires_conversation",
                "Session checkpoints are restored automatically against their exact next conversation prompt.",
                vec!["Continue that conversation to use this checkpoint.".to_string()],
            ),
        ));
    }
    let stable_messages = stable_prompt_messages(&settings, metadata.owner_id.as_deref())?;
    let value = load_persistent_value_or_invalidate(&settings, &metadata)?;
    let compatible = value
        .as_ref()
        .is_some_and(|value| value.is_valid() && value.metadata.fingerprint == expected);
    let equivalent = if let Some(value) = value.as_ref().filter(|_| compatible) {
        verify_restore_equivalence(
            &handle,
            &stable_messages,
            &value.sequence,
            ChatTemplateChoice::ModelDefault,
        )?
    } else {
        false
    };
    if !compatible || !equivalent {
        invalidate_persistent_entry(&settings, &metadata.id)?;
        invalidate_memory(&metadata.id);
        return Ok(CommandResult::blocked(
            "mom_llama.kv_cache_restore",
            "stub_blocked",
            Blocker::new(
                "kv_cache_incompatible",
                "The saved state does not match the current model, template, context, or prompt.",
                vec!["Create a new cache for the current native runtime.".to_string()],
            ),
        ));
    }
    let verified_value = value.ok_or_else(|| anyhow!("compatible cache value disappeared"))?;
    let store = RuntimeStore::open(&settings.data_dir)?;
    let Some(lease) =
        MomCachePromotionLease::acquire(&store, verified_value.metadata.owner_id.as_deref())?
    else {
        return Ok(CommandResult::blocked(
            "mom_llama.kv_cache_restore",
            "stub_blocked",
            Blocker::new(
                "kv_cache_owner_removed",
                "The cache owner was removed before restore admission.",
                vec!["Refresh saved caches and choose an available owner.".to_string()],
            ),
        ));
    };
    let Some(current_value) = load_current_persistent_value(&store, &verified_value.metadata.id)?
    else {
        return Ok(CommandResult::blocked(
            "mom_llama.kv_cache_restore",
            "stub_blocked",
            Blocker::new(
                "kv_cache_not_found",
                "The saved cache changed before restore admission.",
                vec!["Refresh saved caches and try again.".to_string()],
            ),
        ));
    };
    if current_value.sequence != verified_value.sequence
        || !same_cache_authority(&current_value.metadata, &verified_value.metadata)
    {
        return Ok(CommandResult::blocked(
            "mom_llama.kv_cache_restore",
            "stub_blocked",
            Blocker::new(
                "kv_cache_changed",
                "The saved cache changed after verification.",
                vec!["Verify the current cache again.".to_string()],
            ),
        ));
    }
    let Some(value) = touch_persistent_value_for_promotion(&store, &current_value, now_ms())?
    else {
        return Ok(CommandResult::blocked(
            "mom_llama.kv_cache_restore",
            "stub_blocked",
            Blocker::new(
                "kv_cache_owner_removed",
                "The cache owner changed before restore admission.",
                vec!["Refresh saved caches and choose an available owner.".to_string()],
            ),
        ));
    };
    let restore_kind = handle
        .restore_sequence(value.sequence.clone(), 0)
        .map_err(|error| anyhow!(error))?;
    let restored = value.metadata.clone();
    // Native KV is already restored in the model worker. A full persona
    // snapshot can legitimately exceed the separate serialized-state LRU;
    // failure to duplicate it there must not undo a valid disk restore.
    if !lease.promote(value)? && !lease.current_for(restored.owner_id.as_deref())? {
        return Ok(CommandResult::blocked(
            "mom_llama.kv_cache_restore",
            "stub_blocked",
            Blocker::new(
                "kv_cache_owner_removed",
                "The cache owner changed before live restore promotion.",
                vec!["Refresh saved caches and choose an available owner.".to_string()],
            ),
        ));
    }
    Ok(CommandResult::passed(
        "mom_llama.kv_cache_restore",
        "prompt_smoke_verified",
        restored,
        vec![
            settings
                .data_dir
                .join("runtime.sqlite3")
                .display()
                .to_string(),
        ],
        vec![match restore_kind {
            llama_native_types::SequenceRestoreKind::NativeState => {
                "Restored authenticated or live-receipted native KV state.".to_string()
            }
            llama_native_types::SequenceRestoreKind::TokenReplay => {
                "Recomputed the saved prefix from token IDs; no native KV cache hit was claimed."
                    .to_string()
            }
        }],
        true,
        false,
    ))
}

fn clear_checkpoint_store(store: &RuntimeStore) -> Result<()> {
    // Publication and removal use this same authority guard. Invalidate leases
    // before deleting manifests so a delayed writer cannot revive cleared state.
    let _authority = crate::personas::acquire_persona_cache_authority_guard(store)?;
    memory_cache()
        .lock()
        .map_err(|_| anyhow!("cache memory state unavailable"))?
        .clear();
    store.mutate_documents(
        KV_CACHE_NAMESPACE,
        KvCacheDb::default,
        |db: &mut KvCacheDb, documents| {
            for entry in &db.entries {
                delete_cache_bytes(documents, entry);
            }
            db.entries.clear();
            let mut journal = documents
                .get::<CacheStagingJournal>(KV_CACHE_STAGING_NAMESPACE)?
                .unwrap_or_default();
            if let Some(active) = journal.active.take() {
                journal.garbage.push(active);
            }
            documents.put_bytes(KV_CACHE_STAGING_NAMESPACE, &serde_json::to_vec(&journal)?)?;
            Ok(())
        },
    )?;
    Ok(())
}

pub fn kv_cache_clear(scope: &crate::OperationScope) -> Result<CommandResult<KvCacheStatus>> {
    let settings = resolve_settings()?;
    clear_native_prefix_cache(scope, &settings)?;
    let store = RuntimeStore::open(&settings.data_dir)?;
    clear_checkpoint_store(&store)?;
    Ok(CommandResult::passed(
        "mom_llama.kv_cache_clear",
        "contracted",
        KvCacheStatus {
            status: KvCacheState::Invalidated,
            policy: settings.kv_cache_policy,
            cache_dir: "encrypted://runtime.sqlite3".to_string(),
            entries: Vec::new(),
            memory_entries: 0,
            memory_bytes: 0,
            memory_capacity_bytes: DEFAULT_MEMORY_CACHE_BYTES,
            persistent_bytes: 0,
            persistent_capacity_bytes: DEFAULT_PERSISTENT_CACHE_BYTES,
            persistent_capacity_entries: DEFAULT_PERSISTENT_CACHE_ENTRIES,
        },
        vec![
            settings
                .data_dir
                .join("runtime.sqlite3")
                .display()
                .to_string(),
        ],
        Vec::new(),
        false,
        false,
    ))
}

fn restore_latest_session_checkpoint_if_needed(
    settings: &Settings,
    handle: &NativeModelHandle,
    fingerprint: &CacheFingerprint,
    conversation_id: &str,
    expected_revision: u64,
) -> Result<Option<PrefixCacheMetadata>> {
    let store = RuntimeStore::open(&settings.data_dir)?;
    let db = store
        .get::<KvCacheDb>(KV_CACHE_NAMESPACE)?
        .unwrap_or_default();
    let Some(metadata) =
        latest_session_checkpoint(&db.entries, fingerprint, conversation_id).cloned()
    else {
        return Ok(None);
    };

    let resident_cells = if metadata.cells.is_empty() {
        metadata
            .token_ids
            .iter()
            .copied()
            .map(SequenceStateCell::Text)
            .collect()
    } else {
        metadata.cells.clone()
    };
    if handle
        .resident_conversation_covers_cells(resident_cells)
        .map_err(|error| anyhow!(error))?
    {
        return Ok(Some(metadata));
    }

    let Some(lease) = MomCachePromotionLease::acquire(&store, Some(conversation_id))? else {
        return Ok(None);
    };
    let Some(current) = load_current_persistent_value(&store, &metadata.id)? else {
        return Ok(None);
    };
    // Load the large encrypted payload once. `load_current_persistent_value`
    // validates it against the current metadata and invalidates malformed
    // entries; this comparison then ensures the selected checkpoint did not
    // change between the initial selection and the payload read.
    if !same_cache_authority(&current.metadata, &metadata) {
        return Ok(None);
    }
    let Some(value) = touch_persistent_value_for_promotion(&store, &current, now_ms())? else {
        return Ok(None);
    };
    let Some(kind) = handle
        .restore_sequence_if_revision(value.sequence.clone(), 0, expected_revision)
        .map_err(|error| anyhow!(error))?
    else {
        return Ok(None);
    };
    if kind != llama_native_types::SequenceRestoreKind::NativeState {
        anyhow::bail!("session checkpoint did not restore authenticated native KV state");
    }
    let restored = value.metadata.clone();
    if !lease.promote(value)? && !lease.current_for(Some(conversation_id))? {
        anyhow::bail!("session checkpoint owner changed during native restore");
    }
    Ok(Some(restored))
}

fn latest_session_checkpoint<'a>(
    entries: &'a [PrefixCacheMetadata],
    fingerprint: &CacheFingerprint,
    conversation_id: &str,
) -> Option<&'a PrefixCacheMetadata> {
    entries
        .iter()
        .filter(|entry| {
            entry.state == CacheEntryState::Ready
                && entry.tier == CacheTier::SessionPersistent
                && entry.owner_id.as_deref() == Some(conversation_id)
                && &entry.fingerprint == fingerprint
                && (!entry.cells.is_empty() || !entry.token_ids.is_empty())
        })
        .max_by_key(|entry| (entry.last_used_at_ms, entry.created_at_ms))
}

fn compatible_cached_prefix_for_owner(
    handle: &NativeModelHandle,
    messages: &[ChatMessage],
    owner_id: Option<&str>,
) -> Result<Option<(String, SequenceStateBlob)>> {
    let settings = resolve_settings()?;
    compatible_cached_prefix_for_owner_with_settings(
        handle,
        messages,
        owner_id,
        &settings,
        ChatTemplateChoice::ModelDefault,
        0,
    )
}

pub(crate) fn compatible_cached_prefix_for_owner_with_settings(
    handle: &NativeModelHandle,
    messages: &[ChatMessage],
    owner_id: Option<&str>,
    settings: &Settings,
    template: ChatTemplateChoice,
    resident_prefix_tokens: usize,
) -> Result<Option<(String, SequenceStateBlob)>> {
    if messages.is_empty() {
        return Ok(None);
    }
    if !settings.kv_cache_policy.allows_prefix_reuse() {
        return Ok(None);
    }
    let fingerprint = cache_fingerprint(handle)?;
    let tokenized = handle
        .tokenize_messages_with_template(messages.to_vec(), template.clone())
        .map_err(|error| anyhow!(error))?;
    let store = RuntimeStore::open(&settings.data_dir)?;
    let Some(lease) = MomCachePromotionLease::acquire(&store, owner_id)? else {
        return Ok(None);
    };
    let now = now_ms();
    let memory_match = memory_cache().lock().ok().and_then(|state| {
        if state.generation(owner_id) != lease.memory_generation {
            return None;
        }
        match owner_id {
            Some(owner) => {
                state
                    .cache
                    .best_match_for_owner(&fingerprint, &tokenized.token_ids, owner)
            }
            None => state.cache.best_match(&fingerprint, &tokenized.token_ids),
        }
    });
    let db = store
        .get::<KvCacheDb>(KV_CACHE_NAMESPACE)?
        .unwrap_or_default();
    let persistent_match = match owner_id {
        Some(owner) => longest_compatible_prefix_for_owner(
            &db.entries,
            &fingerprint,
            &tokenized.token_ids,
            owner,
        ),
        None => longest_compatible_prefix(&db.entries, &fingerprint, &tokenized.token_ids),
    };
    let selected = choose_match(memory_match, persistent_match);
    let Some(selected) = selected else {
        return Ok(None);
    };
    // Do not decrypt/copy gigabytes merely to pass back a prefix the worker
    // already owns. This is a cache hit in the engine's resident lane.
    if resident_prefix_tokens > 0
        && resident_prefix_tokens
            >= selected
                .matched_tokens
                .min(tokenized.token_ids.len().saturating_sub(1))
    {
        return Ok(None);
    }
    if selected.tier == CacheTier::MemoryLru {
        if !lease.validate()? {
            return Ok(None);
        }
        return Ok(selected_memory_value(
            memory_cache(),
            &selected,
            lease.memory_generation,
            owner_id,
            &fingerprint,
            &tokenized.token_ids,
            now,
        )
        .map(|value| (value.metadata.id, value.sequence)));
    }
    let Some(index) = db.entries.iter().position(|entry| entry.id == selected.id) else {
        return Ok(None);
    };
    let metadata = db.entries[index].clone();
    let Some(value) = load_persistent_value_or_invalidate_from_store(&store, &metadata)? else {
        return Ok(None);
    };
    if !value.is_valid() {
        invalidate_persistent_entry_in_store(&store, &metadata.id)?;
        return Ok(None);
    }
    let Some(value) = touch_persistent_value_for_promotion(&store, &value, now)? else {
        return Ok(None);
    };
    let promoted = lease.promote(value.clone())?;
    if !promoted && !lease.current_for(value.metadata.owner_id.as_deref())? {
        return Ok(None);
    }
    // A valid disk entry need not fit the optional RAM LRU to be reusable.
    Ok(Some((value.metadata.id, value.sequence)))
}

/// Look up a compatible prefix only in the in-process cache. Live voice paths
/// must not open SQLite or wait for the persona cache lease; persistent cache
/// discovery and restoration belongs to the pre-call warm path.
pub(crate) fn compatible_memory_prefix_for_owner_with_settings(
    handle: &NativeModelHandle,
    messages: &[ChatMessage],
    owner_id: &str,
    settings: &Settings,
    template: ChatTemplateChoice,
    resident_prefix_tokens: usize,
) -> Result<Option<(String, SequenceStateBlob)>> {
    if messages.is_empty() || !settings.kv_cache_policy.allows_prefix_reuse() {
        return Ok(None);
    }
    let fingerprint = cache_fingerprint(handle)?;
    let tokenized = handle
        .tokenize_messages_with_template(messages.to_vec(), template)
        .map_err(|error| anyhow!(error))?;
    let Some(value) = memory_cached_prefix(
        memory_cache(),
        owner_id,
        &fingerprint,
        &tokenized.token_ids,
        resident_prefix_tokens,
        now_ms(),
    ) else {
        return Ok(None);
    };
    Ok(Some((value.metadata.id, value.sequence)))
}

fn memory_cached_prefix(
    cache: &Mutex<MemoryCacheState>,
    owner_id: &str,
    fingerprint: &CacheFingerprint,
    prompt_token_ids: &[i32],
    resident_prefix_tokens: usize,
    now: u128,
) -> Option<PrefixCacheValue> {
    let (selected, generation) = {
        let state = cache.lock().ok()?;
        let selected = state
            .cache
            .best_match_for_owner(fingerprint, prompt_token_ids, owner_id)?;
        (selected, state.generation(Some(owner_id)))
    };
    // The resident engine prefix is the longest candidate when it already
    // covers the same token boundary. Returning no blob preserves that state.
    if resident_prefix_tokens
        >= selected
            .matched_tokens
            .min(prompt_token_ids.len().saturating_sub(1))
    {
        return None;
    }
    selected_memory_value(
        cache,
        &selected,
        generation,
        Some(owner_id),
        fingerprint,
        prompt_token_ids,
        now,
    )
}

fn resident_prefix_covers_snapshot(
    resident_text_prefix_tokens: usize,
    resident_media_prefix_tokens: usize,
    prompt_token_ids: &[i32],
    snapshot_token_ids: &[i32],
) -> bool {
    !snapshot_token_ids.is_empty()
        && resident_text_prefix_tokens.max(resident_media_prefix_tokens) >= snapshot_token_ids.len()
        && prompt_token_ids.starts_with(snapshot_token_ids)
}

#[derive(Debug, Clone)]
pub(crate) struct PersonaPrefixUse {
    pub cache_id: String,
    pub sequence: SequenceStateBlob,
}

pub(crate) fn ensure_persona_prefix(
    handle: &NativeModelHandle,
    owner_id: &str,
    target_messages: &[ChatMessage],
) -> Result<Option<PersonaPrefixUse>> {
    let settings = resolve_settings()?;
    if !settings.kv_cache_policy.allows_prefix_reuse() {
        return Ok(None);
    }
    if let Some(value) =
        compatible_cached_prefix_for_owner(handle, target_messages, Some(owner_id))?
    {
        return Ok(Some(PersonaPrefixUse {
            cache_id: value.0,
            sequence: value.1,
        }));
    }
    // A cache miss must remain cheaper than ordinary generation. Gemma 4's
    // hybrid recurrent state exports hundreds of megabytes even for a short
    // system prompt, so eagerly constructing and snapshotting a persona cache
    // delayed every cold reply by several seconds. Explicit cache preparation
    // may still populate a reusable entry; the automatic reply path falls
    // through to direct prompt prefill.
    Ok(None)
}

pub fn kv_cache_checkpoint_committed_voice_session(
    scope: &crate::OperationScope,
    conversation_id: &str,
) -> Result<Option<String>> {
    if let Some(request) = scope.committed_session_request(conversation_id)? {
        schedule_committed_session_checkpoint(scope, conversation_id, &request);
    }
    // This command acknowledges scheduling, never claims a durable cache hit.
    Ok(None)
}

/// Foreground operations only notify the idle worker. They must not serialize
/// another conversation's native state before serving the current request.
pub(crate) fn checkpoint_pending_sessions_before_chat(
    scope: &crate::OperationScope,
    _incoming_conversation_id: &str,
) {
    reschedule_committed_session_checkpoints(scope);
}

pub(crate) fn checkpoint_session_before_profile_change(
    scope: &crate::OperationScope,
    conversation_id: &str,
) {
    let _ = kv_cache_checkpoint_committed_voice_session(scope, conversation_id);
}

pub fn kv_cache_set_voice_call_activity(
    scope: &crate::OperationScope,
    session_id: &str,
    active: bool,
) -> Result<()> {
    scope
        .set_voice_call_activity(session_id, active)
        .map_err(|error| anyhow!("Unable to update voice call activity: {error:#}"))?;
    if !active && !scope.has_active_voice_call() {
        reschedule_committed_session_checkpoints(scope);
    }
    Ok(())
}

fn reschedule_committed_session_checkpoints(scope: &crate::OperationScope) {
    let requests = scope.committed_session_requests().unwrap_or_default();
    for (conversation_id, request_id) in requests {
        schedule_committed_session_checkpoint(scope, &conversation_id, &request_id);
    }
}

/// One immutable snapshot retained by the owned worker across yielded writes.
/// At most one full snapshot per runtime is retained, never one per queued chat.
pub(crate) struct SessionCheckpointProgress {
    conversation_id: String,
    request_id: String,
    store: RuntimeStore,
    value: PrefixCacheValue,
    authority: SessionConversationAuthority,
    memory_generation: MemoryCacheGeneration,
    next_chunk: usize,
    registered: bool,
    max_part_ms: u128,
    snapshot_ms: u128,
    write_started: Instant,
}

enum SessionCheckpointCapture {
    Deferred,
    Obsolete,
    Captured(Box<SessionCheckpointProgress>),
}

fn session_checkpoint_admitted(scope: &crate::OperationScope) -> bool {
    !scope.cancellation_requested()
        && !scope.has_active_voice_call()
        && scope.active_operation_count() == 0
        && !crate::store::store_write_pending()
}

pub(crate) fn run_scheduled_session_checkpoint(
    scope: &crate::OperationScope,
    data_dir: &std::path::Path,
    conversation_id: &str,
    request_id: &str,
    progress: &mut Option<SessionCheckpointProgress>,
) -> Result<bool> {
    if scope.cancellation_requested() {
        return Ok(true);
    }
    if scope.committed_session_request(conversation_id)?.as_deref() != Some(request_id) {
        return Ok(true);
    }
    if !session_checkpoint_admitted(scope) {
        return Ok(false);
    }
    if progress.as_ref().is_some_and(|saved| {
        saved.conversation_id != conversation_id || saved.request_id != request_id
    }) {
        *progress = None;
    }
    if progress.is_none() {
        match capture_session_checkpoint(scope, data_dir, conversation_id, request_id)? {
            SessionCheckpointCapture::Deferred => return Ok(false),
            SessionCheckpointCapture::Obsolete => {
                scope.retire_committed_session_request(conversation_id, request_id)?;
                return Ok(true);
            }
            SessionCheckpointCapture::Captured(saved) => *progress = Some(*saved),
        }
    }
    let Some(saved) = progress.as_mut() else {
        return Ok(false);
    };
    let admitted = || session_checkpoint_admitted(scope);
    if memory_generation(saved.value.metadata.owner_id.as_deref())? != saved.memory_generation {
        scope.retire_committed_session_request(conversation_id, request_id)?;
        *progress = None;
        return Ok(true);
    }
    match advance_session_checkpoint(saved, &admitted)? {
        CheckpointAdvance::Deferred => return Ok(false),
        CheckpointAdvance::Obsolete => {
            scope.retire_committed_session_request(conversation_id, request_id)?;
            *progress = None;
            return Ok(true);
        }
        CheckpointAdvance::Published => {}
    }
    let saved = progress.take().expect("completed checkpoint");
    scope.retire_committed_session_request(conversation_id, request_id)?;
    eprintln!(
        "Blessenger checkpoint: snapshot {} ms, persistence {} ms, bytes {}, parts {}",
        saved.snapshot_ms,
        saved.write_started.elapsed().as_millis(),
        saved.value.metadata.state_bytes,
        saved.next_chunk
    );
    Ok(true)
}

fn capture_session_checkpoint(
    scope: &crate::OperationScope,
    data_dir: &std::path::Path,
    conversation_id: &str,
    request_id: &str,
) -> Result<SessionCheckpointCapture> {
    let started = Instant::now();
    if !session_checkpoint_admitted(scope) {
        return Ok(SessionCheckpointCapture::Deferred);
    }
    let (mut settings, _) = crate::config::resolve_settings_in_directory(data_dir.to_path_buf())?;
    if !settings.kv_cache_policy.persists_conversation_checkpoints() {
        return Ok(SessionCheckpointCapture::Obsolete);
    }
    if scope
        .current_chat_request_id(conversation_id, true)
        .is_some()
    {
        return Ok(SessionCheckpointCapture::Deferred);
    }
    let conversations = crate::conversation_store::load_db_with_settings(&settings)?;
    let Some(conversation) = conversations
        .conversations
        .iter()
        .find(|conversation| conversation.id == conversation_id)
    else {
        return Ok(SessionCheckpointCapture::Obsolete);
    };
    // Fence this snapshot against a newer committed source conversation. The
    // matching check is repeated inside the SQLite mutation that writes the
    // checkpoint, so an older snapshot cannot win a race after a newer chat
    // commit.
    let conversation_authority = SessionConversationAuthority::from_conversation(conversation)?;
    crate::chat::apply_conversation_model_pair(
        &mut settings,
        conversation.execution_profile.model_path.clone(),
        conversation.selected_model_path.clone(),
        conversation.execution_profile.mmproj_path.clone(),
    );
    let Some(model_path) = settings.model_path.clone() else {
        return Ok(SessionCheckpointCapture::Obsolete);
    };
    let store = RuntimeStore::open(&settings.data_dir)?;
    let captured_memory_generation = memory_generation(Some(conversation_id))?;
    let mut source_obsolete = false;
    let snapshot = scope.with_idle_voice_checkpoint(|foreground_waiters| {
        let Some(handle) = resident_model_for_profile_if_loaded(
            scope,
            &settings,
            &model_path,
            settings.mmproj_path.as_deref(),
        )
        .map_err(|blocked| anyhow!(blocked.blocker.message))?
        else {
            source_obsolete = true;
            return Ok(None);
        };
        // An evicted/replaced resident request cannot become snapshot-eligible
        // by polling. Retire it; the last durable prefix remains available.
        if handle
            .inspect_resident_conversation(Some(request_id))
            .map_err(|error| anyhow!(error))?
            .committed_prefix_tokens
            .is_none()
        {
            source_obsolete = true;
            return Ok(None);
        }
        let Some(state) = handle
            .snapshot_conversation_for_request_if_idle(
                &request_id,
                MAX_PERSISTENT_ENTRY_BYTES,
                foreground_waiters,
            )
            .map_err(|error| anyhow!(error))?
        else {
            return Ok(None);
        };
        let fingerprint = handle
            .status()
            .fingerprint
            .ok_or_else(|| anyhow!("resident model has no native fingerprint"))?;
        Ok(Some((state, fingerprint)))
    })?;
    let Some((state, fingerprint)) = snapshot.flatten() else {
        return Ok(if source_obsolete {
            SessionCheckpointCapture::Obsolete
        } else {
            SessionCheckpointCapture::Deferred
        });
    };
    if (state.token_ids.is_empty() && state.cells.is_empty()) || state.bytes.is_empty() {
        return Ok(SessionCheckpointCapture::Obsolete);
    }
    let fingerprint = cache_fingerprint_from_native(fingerprint)?;
    let mut metadata = PrefixCacheMetadata::new(
        Uuid::new_v4().to_string(),
        CacheTier::SessionPersistent,
        fingerprint,
        state.token_ids.clone(),
        state.bytes.len(),
        now_ms(),
    )
    .with_owner(conversation_id)
    .with_label(format!("Conversation {conversation_id}"));
    metadata.cells.clone_from(&state.cells);
    let value = PrefixCacheValue {
        metadata,
        sequence: state,
    };
    if !value.is_valid() {
        return Ok(SessionCheckpointCapture::Obsolete);
    }
    Ok(SessionCheckpointCapture::Captured(Box::new(
        SessionCheckpointProgress {
            conversation_id: conversation_id.to_owned(),
            request_id: request_id.to_owned(),
            store,
            value,
            authority: conversation_authority,
            memory_generation: captured_memory_generation,
            next_chunk: 0,
            registered: false,
            max_part_ms: 0,
            snapshot_ms: started.elapsed().as_millis(),
            write_started: Instant::now(),
        },
    )))
}

pub fn invalidate_cache(cache_id: &str) -> Result<()> {
    let mut db = load_db()?;
    if let Some(entry) = db.entries.iter_mut().find(|entry| entry.id == cache_id) {
        entry.state = CacheEntryState::Invalidated;
        save_db(&db)?;
    }
    invalidate_memory(cache_id);
    Ok(())
}

fn shared_prefix_preparation_request(
    handle: &NativeModelHandle,
    stable_messages: &[ChatMessage],
    owner_id: Option<&str>,
    template: ChatTemplateChoice,
) -> Result<SharedPrefixBatchRequest> {
    let settings = resolve_settings()?;
    let tokenized = handle
        .tokenize_messages_with_template(stable_messages.to_vec(), template.clone())
        .map_err(|error| anyhow!(error))?;
    let cached_prefix = compatible_cached_prefix_for_owner_with_settings(
        handle,
        stable_messages,
        owner_id,
        &settings,
        template.clone(),
        tokenized
            .resident_text_prefix_tokens
            .max(tokenized.resident_media_prefix_tokens),
    )?
    .map(|(_, state)| state);
    let branches = ["alpha", "beta"]
        .into_iter()
        // The shared messages define the prefix even on a single-sequence
        // profile. Cache preparation must respect that profile's admission.
        .take(handle.status().max_sequences as usize)
        .map(|probe| {
            let mut messages = stable_messages.to_vec();
            messages.push(ChatMessage {
                role: ChatRole::User,
                content: format!("{probe} native cache boundary probe"),
            });
            BranchRequest {
                branch_id: format!("cache-probe-{probe}"),
                label: format!("Cache probe {probe}"),
                instruction: String::new(),
                sampling: SamplingConfig::default(),
                messages,
                cached_prefix: None,
            }
        })
        .collect();
    Ok(SharedPrefixBatchRequest {
        request_id: format!("kv-cache-prefill-{}", Uuid::new_v4()),
        model_id: handle.status().model_id,
        common_messages: stable_messages.to_vec(),
        chat_template: template,
        branches,
        cached_prefix,
    })
}

fn create_prefix_value(
    handle: &NativeModelHandle,
    tier: CacheTier,
    owner_id: Option<String>,
    label: String,
    stable_messages: &[ChatMessage],
    template: ChatTemplateChoice,
) -> Result<PrefixCacheValue> {
    let request =
        shared_prefix_preparation_request(handle, stable_messages, owner_id.as_deref(), template)?;
    let state = handle
        .prefill_shared_prefix(request)
        .map_err(|error| anyhow!(error))?;
    let mut metadata = PrefixCacheMetadata::new(
        Uuid::new_v4().to_string(),
        tier,
        cache_fingerprint(handle)?,
        state.token_ids.clone(),
        state.bytes.len(),
        now_ms(),
    )
    .with_label(label);
    if let Some(owner_id) = owner_id {
        metadata = metadata.with_owner(owner_id);
    }
    let value = PrefixCacheValue {
        metadata,
        sequence: state,
    };
    if !value.is_valid() {
        return Err(anyhow!(
            "native prefix state failed its token and byte integrity checks"
        ));
    }
    Ok(value)
}

fn verify_restore_equivalence(
    handle: &NativeModelHandle,
    stable_messages: &[ChatMessage],
    state: &SequenceStateBlob,
    template: ChatTemplateChoice,
) -> Result<bool> {
    let mut messages = stable_messages.to_vec();
    messages.push(ChatMessage {
        role: ChatRole::User,
        content: "Reply with exactly: cache verified".to_string(),
    });
    let request =
        |request_id: String, cached_prefix: Option<SequenceStateBlob>| GenerationRequest {
            request_id,
            model_id: handle.status().model_id,
            input: GenerationInput::Chat {
                messages: messages.clone(),
                template: template.clone(),
            },
            sampling: SamplingConfig {
                seed: 1,
                temperature: 0.0,
                max_tokens: 8,
                ..SamplingConfig::default()
            },
            media: Vec::new(),
            cached_prefix,
        };
    let cached = handle.generate(request(
        format!("kv-cache-restore-{}", Uuid::new_v4()),
        Some(state.clone()),
    ));
    let cached = match cached {
        Ok(ticket) => ticket.wait(),
        Err(error) if error.code == NativeErrorCode::CacheIncompatible => return Ok(false),
        Err(error) => return Err(anyhow!(error)),
    };
    let cached = match cached {
        Ok(outputs) => outputs,
        Err(error) if error.code == NativeErrorCode::CacheIncompatible => return Ok(false),
        Err(error) => return Err(anyhow!(error)),
    };
    let uncached = handle
        .generate(request(
            format!("kv-cache-baseline-{}", Uuid::new_v4()),
            None,
        ))
        .map_err(|error| anyhow!(error))?
        .wait()
        .map_err(|error| anyhow!(error))?;
    let cached = cached.first();
    let uncached = uncached.first();
    Ok(
        cached.is_some_and(|output| output.state == GenerationState::Completed)
            && uncached.is_some_and(|output| output.state == GenerationState::Completed)
            && cached.map(|output| output.text.as_str())
                == uncached.map(|output| output.text.as_str()),
    )
}

fn cache_fingerprint(handle: &NativeModelHandle) -> Result<CacheFingerprint> {
    let fingerprint = handle
        .status()
        .fingerprint
        .ok_or_else(|| anyhow!("resident model has no native fingerprint"))?;
    cache_fingerprint_from_native(fingerprint)
}

fn cache_fingerprint_from_native(
    fingerprint: llama_native_types::ModelFingerprint,
) -> Result<CacheFingerprint> {
    if fingerprint.rope_config_sha256.is_empty() || fingerprint.kv_layout_sha256.is_empty() {
        return Err(anyhow!(
            "resident model lacks the context fingerprints required for safe cache reuse"
        ));
    }
    Ok(CacheFingerprint {
        prompt_form: PromptForm::Chat,
        prompt_token_policy: PromptTokenPolicy::ChatTemplate,
        model_sha256: fingerprint.model_sha256,
        binding_version: fingerprint.binding_version,
        build_id: fingerprint.build_id,
        tokenizer_sha256: fingerprint.tokenizer_sha256,
        chat_template_sha256: fingerprint.chat_template_sha256,
        multimodal_projector_sha256: fingerprint.multimodal_projector_sha256,
        lora_adapters_sha256: Vec::new(),
        context_tokens: fingerprint.context_tokens,
        batch_tokens: fingerprint.batch_tokens,
        max_sequences: fingerprint.max_sequences,
        device: fingerprint.backend,
        rope_config_sha256: fingerprint.rope_config_sha256,
        kv_layout_sha256: fingerprint.kv_layout_sha256,
    })
}

fn stable_prompt_messages(settings: &Settings, owner_id: Option<&str>) -> Result<Vec<ChatMessage>> {
    if let Some(owner_id) = owner_id
        && let Some(persona) = crate::personas::persona_list()?
            .result
            .unwrap_or_default()
            .into_iter()
            .find(|persona| persona.id == owner_id)
    {
        return stable_persona_prompt_messages(settings, &persona);
    }
    let skill_prompt = cache_prompt(owner_id)?;
    let system_message = upstream_setting_string(settings, "systemMessage").unwrap_or_default();
    let combined = [system_message.trim(), skill_prompt.trim()]
        .into_iter()
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    Ok((!combined.is_empty())
        .then_some(ChatMessage {
            role: ChatRole::System,
            content: combined,
        })
        .into_iter()
        .collect())
}

fn stable_persona_prompt_messages(
    settings: &Settings,
    persona: &crate::conversation_store::Conversation,
) -> Result<Vec<ChatMessage>> {
    let system_message = persona
        .execution_profile
        .system_message
        .clone()
        .unwrap_or_else(|| upstream_setting_string(settings, "systemMessage").unwrap_or_default());
    let skill_prompt = crate::skill_store::applied_skill_prompt(&persona.current_skill_ids)?;
    Ok(crate::chat::build_native_messages(
        &system_message,
        &skill_prompt.prompt,
        &[],
        "",
        false,
        &HashMap::new(),
        "",
    )
    .into_iter()
    .filter(|message| message.role == ChatRole::System)
    .collect())
}

fn cache_prompt(skill_id: Option<&str>) -> Result<String> {
    if skill_id.is_none() || skill_id == Some(BUILTIN_CACHE_OWNER) {
        return Ok(BUILTIN_CACHE_PROMPT.to_string());
    }
    if let Some(skill_id) = skill_id {
        let db = load_skill_db()?;
        if let Some(skill) = db
            .skills
            .iter()
            .find(|skill| skill.id == skill_id || skill.name == skill_id)
        {
            return Ok(skill.prompt_template.clone());
        }
    }
    Ok(String::new())
}

fn skill_label(skill_id: Option<&str>) -> Result<String> {
    if let Some(skill_id) = skill_id {
        if let Some(persona) = crate::personas::persona_list()?
            .result
            .unwrap_or_default()
            .into_iter()
            .find(|persona| persona.id == skill_id)
        {
            return Ok(format!("Persona: {}", persona.title));
        }
        let db = load_skill_db()?;
        if let Some(skill) = db
            .skills
            .iter()
            .find(|skill| skill.id == skill_id || skill.name == skill_id)
        {
            return Ok(format!("Persona: {}", skill.name));
        }
    }
    Ok("Built-in kind local assistant".to_string())
}

struct MomCachePromotionLease {
    owner: Option<crate::personas::PersonaCacheOwnerLease>,
    memory_generation: MemoryCacheGeneration,
}

impl MomCachePromotionLease {
    fn acquire(store: &RuntimeStore, owner_id: Option<&str>) -> Result<Option<Self>> {
        let owner = match owner_id {
            Some(owner_id) => {
                let Some(lease) =
                    crate::personas::acquire_persona_cache_owner_lease(store, owner_id)?
                else {
                    return Ok(None);
                };
                Some(lease)
            }
            None => None,
        };
        let memory_generation = memory_generation(owner_id)?;
        Ok(Some(Self {
            owner,
            memory_generation,
        }))
    }

    fn try_acquire(store: &RuntimeStore, owner_id: &str) -> Result<Option<Self>> {
        let Some(owner) = crate::personas::try_acquire_persona_cache_owner_lease(store, owner_id)?
        else {
            return Ok(None);
        };
        Ok(Some(Self {
            owner: Some(owner),
            memory_generation: memory_generation(Some(owner_id))?,
        }))
    }

    fn validate(&self) -> Result<bool> {
        match &self.owner {
            Some(owner) => owner.validate(),
            None => Ok(true),
        }
    }

    fn current_for(&self, owner_id: Option<&str>) -> Result<bool> {
        Ok(self.validate()? && memory_generation(owner_id)? == self.memory_generation)
    }

    fn promote(&self, value: PrefixCacheValue) -> Result<bool> {
        if !self.validate()? {
            return Ok(false);
        }
        promote_to_memory_if_current(value, self.memory_generation)
    }
}

fn persist_value(settings: &Settings, value: &PrefixCacheValue) -> Result<bool> {
    let store = RuntimeStore::open(&settings.data_dir)?;
    persist_value_with_promotion_hook(&store, value, || {})
}

#[derive(Debug, Clone, PartialEq)]
struct SessionConversationAuthority {
    conversation_id: String,
    conversation: crate::conversation_store::Conversation,
}

impl SessionConversationAuthority {
    fn from_conversation(conversation: &crate::conversation_store::Conversation) -> Result<Self> {
        Ok(Self {
            conversation_id: conversation.id.clone(),
            conversation: conversation.clone(),
        })
    }

    fn still_authoritative_in_documents(
        &self,
        documents: &DocumentMutations<'_, '_, '_>,
    ) -> Result<bool> {
        let Some(db) = documents.get::<crate::conversation_store::ConversationDb>(
            crate::conversation_store::CONVERSATIONS_NAMESPACE,
        )?
        else {
            return Ok(false);
        };
        let Some(conversation) = db
            .conversations
            .iter()
            .find(|conversation| conversation.id == self.conversation_id)
        else {
            return Ok(false);
        };
        if conversation == &self.conversation {
            return Ok(true);
        }

        let expected_voice_replies = crate::attachments::generated_voice_reply_ids_from_mutations(
            documents,
            &self.conversation_id,
            &self.conversation.messages,
        )?;
        let current_voice_replies = crate::attachments::generated_voice_reply_ids_from_mutations(
            documents,
            &self.conversation_id,
            &conversation.messages,
        )?;
        if !current_voice_replies.is_superset(&expected_voice_replies) {
            return Ok(false);
        }
        if current_voice_replies == expected_voice_replies {
            return Ok(false);
        }

        // Saving a generated assistant WAV updates only its attachment ID and
        // the conversation timestamp. Treat precisely that authenticated,
        // additive publication as non-inference metadata; compare every other
        // conversation field and attachment ID normally.
        let published_pairs = current_voice_replies
            .difference(&expected_voice_replies)
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        let mut normalized_expected = self.conversation.clone();
        let mut normalized_current = conversation.clone();
        normalized_expected
            .updated_at
            .clone_from(&normalized_current.updated_at);
        for item in [&mut normalized_expected, &mut normalized_current] {
            for message in &mut item.messages {
                message.attachment_ids.retain(|attachment_id| {
                    !published_pairs.iter().any(|(message_id, published_id)| {
                        message_id == &message.id && published_id == attachment_id
                    })
                });
            }
        }
        Ok(normalized_current == normalized_expected)
    }
}

fn persist_session_checkpoint_value(
    scope: &crate::OperationScope,
    settings: &Settings,
    value: &PrefixCacheValue,
    authority: &SessionConversationAuthority,
) -> Result<bool> {
    let store = RuntimeStore::open(&settings.data_dir)?;
    persist_value_with_authority_and_promotion_hook(
        &store,
        value,
        Some(authority),
        &|| {
            !scope.cancellation_requested()
                && !scope.has_active_voice_call()
                && scope.active_operation_count() == 0
                && !crate::store::store_write_pending()
        },
        || {},
    )
}

// Staged parts are encrypted storage documents, but never advertised as a
// usable checkpoint until their manifest is published atomically. One staging
// slot per store bounds abandoned writes. A new writer fences out an older
// one and reclaims its parts in bounded transactions before writing new parts.
const KV_CACHE_STAGING_NAMESPACE: &str = "kv-cache.v3.staging";

#[derive(Default, Serialize, Deserialize)]
struct CacheStagingJournal {
    active: Option<StagedCacheParts>,
    garbage: Vec<StagedCacheParts>,
}

#[derive(Serialize, Deserialize)]
struct StagedCacheParts {
    id: String,
    written: usize,
    deleted: usize,
}

impl StagedCacheParts {
    fn published(metadata: &PrefixCacheMetadata) -> Self {
        Self {
            id: metadata.id.clone(),
            written: metadata
                .state_bytes
                .min(MAX_PERSISTENT_ENTRY_BYTES)
                .div_ceil(CACHE_BLOB_CHUNK_BYTES)
                .max(1),
            deleted: 0,
        }
    }
}

#[derive(Debug)]
enum CheckpointAdvance {
    Published,
    Deferred,
    Obsolete,
}

fn advance_session_checkpoint(
    saved: &mut SessionCheckpointProgress,
    admitted: &dyn Fn() -> bool,
) -> Result<CheckpointAdvance> {
    let result = advance_session_checkpoint_inner(saved, admitted);
    match result {
        Err(error) if error.is::<CachePersistencePreempted>() => Ok(CheckpointAdvance::Deferred),
        other => other,
    }
}

fn advance_session_checkpoint_inner(
    saved: &mut SessionCheckpointProgress,
    admitted: &dyn Fn() -> bool,
) -> Result<CheckpointAdvance> {
    require_cache_persistence_admission(admitted)?;
    if saved.value.metadata.reconstruction.is_none() {
        let started = Instant::now();
        let Some(reconstruction) = saved.value.sequence.reconstruction_while(admitted) else {
            return Ok(CheckpointAdvance::Deferred);
        };
        saved.value.metadata.reconstruction = Some(reconstruction);
        eprintln!(
            "Blessenger checkpoint: reconstruction {} ms",
            started.elapsed().as_millis()
        );
    }
    if !saved.value.is_valid() || !persistent_entry_fits(saved.value.metadata.state_bytes) {
        anyhow::bail!("refusing invalid or oversized staged native checkpoint");
    }
    if !saved.registered {
        saved.store.mutate_documents_with_admission(
            KV_CACHE_STAGING_NAMESPACE,
            CacheStagingJournal::default,
            || require_cache_persistence_admission(admitted),
            |journal, _| {
                if let Some(previous) = journal.active.take() {
                    journal.garbage.push(previous);
                }
                journal.active = Some(StagedCacheParts {
                    id: saved.value.metadata.id.clone(),
                    written: 0,
                    deleted: 0,
                });
                Ok(())
            },
        )?;
        saved.registered = true;
    }
    // Old versions and interrupted writes are reclaimed by idle work, never
    // by quit or foreground navigation. Each commit holds only one part.
    reclaim_staged_cache_garbage(&saved.store, admitted)?;
    for (index, chunk) in saved
        .value
        .sequence
        .bytes
        .chunks(CACHE_BLOB_CHUNK_BYTES)
        .enumerate()
        .skip(saved.next_chunk)
    {
        let started = Instant::now();
        let written = saved.store.mutate_documents_with_admission(
            KV_CACHE_STAGING_NAMESPACE,
            CacheStagingJournal::default,
            || require_cache_persistence_admission(admitted),
            |journal, documents| {
                let Some(active) = journal.active.as_mut().filter(|active| {
                    active.id == saved.value.metadata.id && active.written == index
                }) else {
                    return Ok(false);
                };
                documents.put_bytes_streamed(&blob_chunk_namespace(&active.id, index), chunk)?;
                active.written += 1;
                Ok(true)
            },
        )?;
        if !written {
            return Ok(CheckpointAdvance::Obsolete);
        }
        saved.next_chunk = index + 1;
        saved.max_part_ms = saved.max_part_ms.max(started.elapsed().as_millis());
    }
    require_cache_persistence_admission(admitted)?;
    if memory_generation(saved.value.metadata.owner_id.as_deref())? != saved.memory_generation {
        return Ok(CheckpointAdvance::Obsolete);
    }
    let Some(lease) = MomCachePromotionLease::try_acquire(&saved.store, &saved.conversation_id)?
    else {
        return Ok(CheckpointAdvance::Deferred);
    };
    if lease.memory_generation != saved.memory_generation {
        return Ok(CheckpointAdvance::Obsolete);
    }
    let started = Instant::now();
    let published = saved.store.mutate_documents_with_admission(
        KV_CACHE_NAMESPACE,
        KvCacheDb::default,
        || require_cache_persistence_admission(admitted),
        |db, documents| {
            let mut journal = documents
                .get::<CacheStagingJournal>(KV_CACHE_STAGING_NAMESPACE)?
                .unwrap_or_default();
            let complete = journal.active.as_ref().is_some_and(|active| {
                active.id == saved.value.metadata.id && active.written == saved.next_chunk
            });
            if !complete
                || !saved
                    .authority
                    .still_authoritative_in_documents(documents)?
            {
                return Ok(false);
            }
            if let Some(owner_id) = saved.value.metadata.owner_id.as_deref()
                && crate::personas::persona_cache_owner_is_removed_from_documents(
                    documents, owner_id,
                )?
            {
                return Ok(false);
            }
            let value = &saved.value;
            db.entries.retain(|entry| {
                let replaced = entry.id == value.metadata.id
                    || (entry.tier == value.metadata.tier
                        && entry.owner_id == value.metadata.owner_id
                        && entry.fingerprint == value.metadata.fingerprint);
                if replaced {
                    journal.garbage.push(StagedCacheParts::published(entry));
                }
                !replaced
            });
            db.entries.insert(0, value.metadata.clone());
            let evicted = persistent_eviction_ids(
                &db.entries,
                &value.metadata.id,
                DEFAULT_PERSISTENT_CACHE_ENTRIES,
                DEFAULT_PERSISTENT_CACHE_BYTES,
            );
            db.entries.retain(|entry| {
                if evicted.contains(&entry.id) {
                    journal.garbage.push(StagedCacheParts::published(entry));
                    false
                } else {
                    true
                }
            });
            journal.active = None;
            documents.put_bytes(KV_CACHE_STAGING_NAMESPACE, &serde_json::to_vec(&journal)?)?;
            Ok(true)
        },
    )?;
    if !published {
        return Ok(CheckpointAdvance::Obsolete);
    }
    eprintln!(
        "Blessenger checkpoint: publication {} ms, largest part {} ms",
        started.elapsed().as_millis(),
        saved.max_part_ms
    );
    if !lease.promote(saved.value.clone())?
        && !lease.current_for(saved.value.metadata.owner_id.as_deref())?
    {
        // Fence cache-clear/removal races without synchronously deleting GiB
        // of parts. Reclamation belongs to the next admitted idle writer.
        saved
            .store
            .mutate_documents(KV_CACHE_NAMESPACE, KvCacheDb::default, |db, documents| {
                let mut journal = documents
                    .get::<CacheStagingJournal>(KV_CACHE_STAGING_NAMESPACE)?
                    .unwrap_or_default();
                db.entries.retain(|entry| {
                    if entry.id == saved.value.metadata.id {
                        journal.garbage.push(StagedCacheParts::published(entry));
                        false
                    } else {
                        true
                    }
                });
                documents.put_bytes(KV_CACHE_STAGING_NAMESPACE, &serde_json::to_vec(&journal)?)?;
                Ok(())
            })?;
        return Ok(CheckpointAdvance::Obsolete);
    }
    drop(lease);
    // Reclaim the previous version while still idle, releasing storage between
    // parts. Foreground preemption may leave it for the next idle save; quit
    // never waits to drain this garbage.
    if let Err(error) = reclaim_staged_cache_garbage(&saved.store, admitted)
        && !error.is::<CachePersistencePreempted>()
    {
        eprintln!("Idle cache reclamation deferred after publication: {error:#}");
    }
    Ok(CheckpointAdvance::Published)
}

fn reclaim_staged_cache_garbage(store: &RuntimeStore, admitted: &dyn Fn() -> bool) -> Result<()> {
    loop {
        require_cache_persistence_admission(admitted)?;
        let remains = store.mutate_documents_with_admission(
            KV_CACHE_STAGING_NAMESPACE,
            CacheStagingJournal::default,
            || require_cache_persistence_admission(admitted),
            |journal, documents| {
                journal
                    .garbage
                    .retain(|parts| parts.deleted < parts.written);
                let Some(parts) = journal.garbage.first_mut() else {
                    return Ok(false);
                };
                anyhow::ensure!(
                    parts.written <= MAX_PERSISTENT_ENTRY_BYTES.div_ceil(CACHE_BLOB_CHUNK_BYTES),
                    "invalid staged cache part count"
                );
                documents.delete(&blob_chunk_namespace(&parts.id, parts.deleted));
                parts.deleted += 1;
                Ok(true)
            },
        )?;
        if !remains {
            return Ok(());
        }
    }
}

fn persistent_entry_fits(bytes: usize) -> bool {
    bytes > 0 && bytes <= MAX_PERSISTENT_ENTRY_BYTES
}

fn persist_value_with_promotion_hook(
    store: &RuntimeStore,
    value: &PrefixCacheValue,
    before_promotion: impl FnOnce(),
) -> Result<bool> {
    persist_value_with_authority_and_promotion_hook(store, value, None, &|| true, before_promotion)
}

fn persist_value_with_authority_and_promotion_hook(
    store: &RuntimeStore,
    value: &PrefixCacheValue,
    authority: Option<&SessionConversationAuthority>,
    admitted: &dyn Fn() -> bool,
    before_promotion: impl FnOnce(),
) -> Result<bool> {
    if !admitted() {
        return Ok(false);
    }
    let Some(lease) = MomCachePromotionLease::acquire(store, value.metadata.owner_id.as_deref())?
    else {
        return Ok(false);
    };
    if !persist_value_to_store_with_admission(
        store,
        value,
        DEFAULT_PERSISTENT_CACHE_ENTRIES,
        DEFAULT_PERSISTENT_CACHE_BYTES,
        authority,
        admitted,
    )? {
        return Ok(false);
    }
    before_promotion();
    if lease.promote(value.clone())? || lease.current_for(value.metadata.owner_id.as_deref())? {
        return Ok(true);
    }
    invalidate_persistent_entry_in_store(store, &value.metadata.id)?;
    Ok(false)
}

#[cfg(test)]
fn persist_value_to_store(
    store: &RuntimeStore,
    value: &PrefixCacheValue,
    max_entries: usize,
    max_bytes: usize,
) -> Result<bool> {
    persist_value_to_store_with_authority(store, value, max_entries, max_bytes, None)
}

#[cfg(test)]
fn persist_value_to_store_with_authority(
    store: &RuntimeStore,
    value: &PrefixCacheValue,
    max_entries: usize,
    max_bytes: usize,
    authority: Option<&SessionConversationAuthority>,
) -> Result<bool> {
    persist_value_to_store_with_admission(store, value, max_entries, max_bytes, authority, &|| true)
}

#[derive(Debug)]
struct CachePersistencePreempted;

impl std::fmt::Display for CachePersistencePreempted {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("cache persistence yielded to a voice call")
    }
}
impl std::error::Error for CachePersistencePreempted {}

fn require_cache_persistence_admission(admitted: &dyn Fn() -> bool) -> Result<()> {
    if admitted() {
        Ok(())
    } else {
        Err(CachePersistencePreempted.into())
    }
}

fn persist_value_to_store_with_admission(
    store: &RuntimeStore,
    value: &PrefixCacheValue,
    max_entries: usize,
    max_bytes: usize,
    authority: Option<&SessionConversationAuthority>,
    admitted: &dyn Fn() -> bool,
) -> Result<bool> {
    if !admitted() {
        return Ok(false);
    }
    let mut persisted = value.clone();
    let Some(reconstruction) = persisted.sequence.reconstruction_while(admitted) else {
        return Ok(false);
    };
    persisted
        .metadata
        .cells
        .clone_from(&persisted.sequence.cells);
    persisted.metadata.reconstruction = Some(reconstruction);
    let value = &persisted;
    if !value.is_valid() {
        return Err(anyhow!(
            "refusing to persist an invalid native prefix cache"
        ));
    }
    if !persistent_entry_fits(value.metadata.state_bytes) {
        return Err(anyhow!(
            "native prefix state exceeds the per-entry persistent cache limit"
        ));
    }
    if value.metadata.state_bytes > max_bytes {
        return Err(anyhow!(
            "native prefix state exceeds the persistent cache byte budget"
        ));
    }
    let result = store.mutate_documents_with_admission(
        KV_CACHE_NAMESPACE,
        KvCacheDb::default,
        || require_cache_persistence_admission(admitted),
        |db: &mut KvCacheDb, documents| {
            require_cache_persistence_admission(admitted)?;
            if let Some(authority) = authority
                && !authority.still_authoritative_in_documents(documents)?
            {
                return Ok(false);
            }
            if let Some(owner_id) = value.metadata.owner_id.as_deref()
                && crate::personas::persona_cache_owner_is_removed_from_documents(
                    documents, owner_id,
                )?
            {
                return Ok(false);
            }
            let replaced_ids = db
                .entries
                .iter()
                .filter(|entry| {
                    entry.id == value.metadata.id
                        || (entry.tier == value.metadata.tier
                            && entry.owner_id == value.metadata.owner_id
                            && entry.fingerprint == value.metadata.fingerprint)
                })
                .cloned()
                .collect::<Vec<_>>();
            for entry in replaced_ids {
                delete_cache_bytes(documents, &entry);
            }
            // Yield at every bounded encrypted part. An error rolls back this
            // entire transaction, including replacement/eviction; the existing
            // durable checkpoint stays intact and the pending request can retry.
            write_cache_bytes_with_admission(
                documents,
                &value.metadata.id,
                &value.sequence.bytes,
                admitted,
            )?;
            db.entries.retain(|entry| {
                entry.id != value.metadata.id
                    && !(entry.tier == value.metadata.tier
                        && entry.owner_id == value.metadata.owner_id
                        && entry.fingerprint == value.metadata.fingerprint)
            });
            db.entries.insert(0, value.metadata.clone());
            let evicted =
                persistent_eviction_ids(&db.entries, &value.metadata.id, max_entries, max_bytes);
            for entry in db
                .entries
                .iter()
                .filter(|entry| evicted.contains(&entry.id))
            {
                delete_cache_bytes(documents, entry);
            }
            db.entries.retain(|entry| !evicted.contains(&entry.id));
            Ok(true)
        },
    );
    match result {
        Err(error) if error.is::<CachePersistencePreempted>() => Ok(false),
        other => other,
    }
}

pub(crate) fn persona_cache_ids_from_snapshot(
    snapshot: &DocumentSnapshot<'_, '_, '_>,
    owner_id: &str,
) -> Result<Vec<String>> {
    let db = snapshot
        .get::<KvCacheDb>(KV_CACHE_NAMESPACE)?
        .unwrap_or_default();
    Ok(cache_ids_for_owner(&db, owner_id))
}

pub(crate) fn persona_cache_ids_from_documents(
    documents: &DocumentMutations<'_, '_, '_>,
    owner_id: &str,
) -> Result<Vec<String>> {
    let db = documents
        .get::<KvCacheDb>(KV_CACHE_NAMESPACE)?
        .unwrap_or_default();
    Ok(cache_ids_for_owner(&db, owner_id))
}

pub(crate) fn remove_persona_cache_from_documents(
    documents: &mut DocumentMutations<'_, '_, '_>,
    owner_id: &str,
) -> Result<Vec<String>> {
    let mut db = documents
        .get::<KvCacheDb>(KV_CACHE_NAMESPACE)?
        .unwrap_or_default();
    let removed = cache_ids_for_owner(&db, owner_id);
    for entry in db
        .entries
        .iter()
        .filter(|entry| removed.contains(&entry.id))
    {
        delete_cache_bytes(documents, entry);
    }
    db.entries
        .retain(|entry| entry.owner_id.as_deref() != Some(owner_id));
    documents.put_bytes(KV_CACHE_NAMESPACE, &serde_json::to_vec(&db)?)?;
    Ok(removed)
}

pub(crate) fn invalidate_persona_memory_cache(owner_id: &str) -> Result<usize> {
    let removed = memory_cache()
        .lock()
        .map_err(|_| anyhow!("Persona prefix memory cache is unavailable"))?
        .invalidate_owner(owner_id);
    Ok(removed)
}

fn cache_ids_for_owner(db: &KvCacheDb, owner_id: &str) -> Vec<String> {
    let mut ids = db
        .entries
        .iter()
        .filter(|entry| entry.owner_id.as_deref() == Some(owner_id))
        .map(|entry| entry.id.clone())
        .collect::<Vec<_>>();
    ids.sort();
    ids
}

fn load_persistent_value_or_invalidate(
    settings: &Settings,
    metadata: &PrefixCacheMetadata,
) -> Result<Option<PrefixCacheValue>> {
    let store = RuntimeStore::open(&settings.data_dir)?;
    load_persistent_value_or_invalidate_from_store(&store, metadata)
}

fn load_persistent_value_or_invalidate_from_store(
    store: &RuntimeStore,
    metadata: &PrefixCacheMetadata,
) -> Result<Option<PrefixCacheValue>> {
    let Some(reconstruction) = metadata.reconstruction.as_ref() else {
        return Ok(None);
    };
    // This process accepts only the current authenticated storage format.
    // Earlier or unknown snapshots stay untouched and fall back to prompt replay.
    if reconstruction.header.len() != llama_native_types::SEQUENCE_STATE_HEADER_BYTES
        || !reconstruction
            .header
            .starts_with(llama_native_types::SEQUENCE_STATE_MAGIC)
        || metadata.state_bytes < llama_native_types::SEQUENCE_STATE_HEADER_BYTES
    {
        return Ok(None);
    }
    if reconstruction.token_replay_allowed != metadata.cells.is_empty() {
        invalidate_persistent_entry_if_matches_in_store(store, metadata)?;
        return Ok(None);
    }
    // Both current metadata and all payload chunks are verified from one AEAD
    // read snapshot; only then is the process-local storage proof minted.
    let snapshot_payload = store.read_documents(|snapshot| {
        let db = snapshot
            .get::<KvCacheDb>(KV_CACHE_NAMESPACE)?
            .unwrap_or_default();
        if !db.entries.iter().any(|entry| entry == metadata) {
            return Ok(None);
        }
        Ok(Some(read_cache_bytes_from_snapshot(snapshot, metadata)?))
    });
    let bytes = match snapshot_payload {
        Ok(Some(Some(bytes))) => bytes,
        Ok(None) => return Ok(None),
        Ok(Some(None)) | Err(_) => {
            invalidate_persistent_entry_if_matches_in_store(store, metadata)?;
            return Ok(None);
        }
    };
    let mut value = PrefixCacheValue {
        metadata: metadata.clone(),
        sequence: SequenceStateBlob {
            storage_proof: None,
            sequence_id: reconstruction.sequence_id,
            token_count: if metadata.cells.is_empty() {
                metadata.token_ids.len()
            } else {
                metadata.cells.len()
            },
            bytes: bytes.into(),
            token_ids: metadata.token_ids.clone(),
            cells: metadata.cells.clone(),
        },
    };
    if !value.is_valid() {
        invalidate_persistent_entry_if_matches_in_store(store, metadata)?;
        return Ok(None);
    }
    // Both metadata and every payload chunk came from authenticated documents.
    // This non-serialized proof binds their exact immutable allocation.
    value.sequence.mark_authenticated_storage();
    Ok(Some(value))
}

fn load_current_persistent_value(
    store: &RuntimeStore,
    cache_id: &str,
) -> Result<Option<PrefixCacheValue>> {
    let db = store
        .get::<KvCacheDb>(KV_CACHE_NAMESPACE)?
        .unwrap_or_default();
    let Some(metadata) = db.entries.iter().find(|entry| entry.id == cache_id) else {
        return Ok(None);
    };
    load_persistent_value_or_invalidate_from_store(store, metadata)
}

fn touch_persistent_value_for_promotion(
    store: &RuntimeStore,
    value: &PrefixCacheValue,
    now: u128,
) -> Result<Option<PrefixCacheValue>> {
    let metadata = store.mutate_documents(
        KV_CACHE_NAMESPACE,
        KvCacheDb::default,
        |db: &mut KvCacheDb, documents| {
            if let Some(owner_id) = value.metadata.owner_id.as_deref()
                && crate::personas::persona_cache_owner_is_removed_from_documents(
                    documents, owner_id,
                )?
            {
                return Ok(None);
            }
            let Some(metadata) = db
                .entries
                .iter_mut()
                .find(|entry| entry.id == value.metadata.id)
            else {
                return Ok(None);
            };
            if !same_cache_authority(metadata, &value.metadata) {
                return Ok(None);
            }
            metadata.last_used_at_ms = now;
            Ok(Some(metadata.clone()))
        },
    )?;
    Ok(metadata.map(|metadata| PrefixCacheValue {
        metadata,
        sequence: value.sequence.clone(),
    }))
}

fn same_cache_authority(left: &PrefixCacheMetadata, right: &PrefixCacheMetadata) -> bool {
    left.id == right.id
        && left.tier == right.tier
        && left.owner_id == right.owner_id
        && left.label == right.label
        && left.fingerprint == right.fingerprint
        && left.token_ids == right.token_ids
        && left.cells == right.cells
        && left.state_bytes == right.state_bytes
        && left.created_at_ms == right.created_at_ms
        && left.state == right.state
        && left.reconstruction == right.reconstruction
}

fn invalidate_persistent_entry(settings: &Settings, cache_id: &str) -> Result<()> {
    invalidate_persistent_entry_in_store(&RuntimeStore::open(&settings.data_dir)?, cache_id)
}

fn invalidate_persistent_entry_in_store(store: &RuntimeStore, cache_id: &str) -> Result<()> {
    store.mutate_documents(
        KV_CACHE_NAMESPACE,
        KvCacheDb::default,
        |db: &mut KvCacheDb, documents| {
            if let Some(entry) = db.entries.iter_mut().find(|entry| entry.id == cache_id) {
                entry.state = CacheEntryState::Invalidated;
                delete_cache_bytes(documents, entry);
            }
            Ok(())
        },
    )
}

fn invalidate_persistent_entry_if_matches_in_store(
    store: &RuntimeStore,
    expected: &PrefixCacheMetadata,
) -> Result<()> {
    store.mutate_documents(
        KV_CACHE_NAMESPACE,
        KvCacheDb::default,
        |db: &mut KvCacheDb, documents| {
            if let Some(index) = db.entries.iter().position(|entry| entry == expected) {
                let entry = &mut db.entries[index];
                entry.state = CacheEntryState::Invalidated;
                delete_cache_bytes(documents, entry);
            }
            Ok(())
        },
    )
}

fn persistent_eviction_ids(
    entries: &[PrefixCacheMetadata],
    protected_id: &str,
    max_entries: usize,
    max_bytes: usize,
) -> Vec<String> {
    let mut evicted = entries
        .iter()
        .filter(|entry| entry.state == CacheEntryState::Invalidated)
        .map(|entry| entry.id.clone())
        .collect::<Vec<_>>();
    let mut ready_count = entries
        .iter()
        .filter(|entry| entry.state == CacheEntryState::Ready)
        .count();
    let mut ready_bytes = entries
        .iter()
        .filter(|entry| entry.state == CacheEntryState::Ready)
        .fold(0_usize, |total, entry| {
            total.saturating_add(entry.state_bytes)
        });
    let mut candidates = entries
        .iter()
        .filter(|entry| entry.state == CacheEntryState::Ready && entry.id != protected_id)
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        left.last_used_at_ms
            .cmp(&right.last_used_at_ms)
            .then_with(|| left.created_at_ms.cmp(&right.created_at_ms))
            .then_with(|| left.id.cmp(&right.id))
    });
    for entry in candidates {
        if ready_count <= max_entries && ready_bytes <= max_bytes {
            break;
        }
        evicted.push(entry.id.clone());
        ready_count = ready_count.saturating_sub(1);
        ready_bytes = ready_bytes.saturating_sub(entry.state_bytes);
    }
    evicted
}

fn load_db() -> Result<KvCacheDb> {
    let settings = resolve_settings()?;
    Ok(RuntimeStore::open(&settings.data_dir)?
        .get(KV_CACHE_NAMESPACE)?
        .unwrap_or_default())
}

fn save_db(db: &KvCacheDb) -> Result<PathBuf> {
    let settings = resolve_settings()?;
    let store = RuntimeStore::open(&settings.data_dir)?;
    store.put(KV_CACHE_NAMESPACE, db)?;
    Ok(store.path().to_path_buf())
}

fn memory_generation(owner_id: Option<&str>) -> Result<MemoryCacheGeneration> {
    Ok(memory_cache()
        .lock()
        .map_err(|_| anyhow!("Persona prefix memory cache is unavailable"))?
        .generation(owner_id))
}

fn selected_memory_value(
    cache: &Mutex<MemoryCacheState>,
    selected: &CacheMatch,
    expected_generation: MemoryCacheGeneration,
    owner_id: Option<&str>,
    fingerprint: &CacheFingerprint,
    prompt_token_ids: &[i32],
    now: u128,
) -> Option<PrefixCacheValue> {
    let mut state = cache.lock().ok()?;
    if state.generation(owner_id) != expected_generation {
        return None;
    }
    let shared = state.cache.get_shared(&selected.id, now)?;
    drop(state);
    let mut value = (*shared).clone();
    value.metadata.last_used_at_ms = now;
    let matched_tokens =
        llama_native_cache::common_prefix_tokens(&value.metadata.token_ids, prompt_token_ids);
    (value.metadata.id == selected.id
        && value.metadata.tier == selected.tier
        && value.metadata.owner_id.as_deref() == owner_id
        && &value.metadata.fingerprint == fingerprint
        && matched_tokens == selected.matched_tokens
        && matched_tokens < prompt_token_ids.len()
        && matched_tokens > 0
        && selected.exact == (matched_tokens.saturating_add(1) == prompt_token_ids.len()))
    .then_some(value)
}

fn promote_to_memory_if_current(
    value: PrefixCacheValue,
    expected: MemoryCacheGeneration,
) -> Result<bool> {
    Ok(memory_cache()
        .lock()
        .map_err(|_| anyhow!("Persona prefix memory cache is unavailable"))?
        .promote_if_current(value, expected))
}

pub(crate) fn invalidate_memory(cache_id: &str) {
    if let Ok(mut state) = memory_cache().lock() {
        state.global_generation = state.global_generation.saturating_add(1);
        state.cache.invalidate(cache_id);
    }
}

fn memory_totals() -> (usize, usize, usize) {
    memory_cache()
        .lock()
        .map(|state| {
            (
                state.cache.len(),
                state.cache.used_bytes(),
                state.cache.capacity_bytes(),
            )
        })
        .unwrap_or((0, 0, DEFAULT_MEMORY_CACHE_BYTES))
}

fn choose_match(memory: Option<CacheMatch>, persistent: Option<CacheMatch>) -> Option<CacheMatch> {
    match (memory, persistent) {
        (Some(memory), Some(persistent)) => {
            if memory.matched_tokens >= persistent.matched_tokens {
                Some(memory)
            } else {
                Some(persistent)
            }
        }
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn blob_namespace(id: &str) -> String {
    format!("{KV_CACHE_BLOB_PREFIX}{id}")
}

fn blob_chunk_namespace(id: &str, index: usize) -> String {
    if index == 0 {
        blob_namespace(id)
    } else {
        format!("{}.part-{index}", blob_namespace(id))
    }
}

fn write_cache_bytes_with_admission(
    documents: &mut DocumentMutations<'_, '_, '_>,
    id: &str,
    bytes: &[u8],
    admitted: &dyn Fn() -> bool,
) -> Result<()> {
    if !persistent_entry_fits(bytes.len()) {
        return Err(anyhow!(
            "native prefix state exceeds the per-entry persistent cache limit"
        ));
    }
    for (index, chunk) in bytes.chunks(CACHE_BLOB_CHUNK_BYTES).enumerate() {
        require_cache_persistence_admission(admitted)?;
        documents.put_bytes_streamed(&blob_chunk_namespace(id, index), chunk)?;
    }
    require_cache_persistence_admission(admitted)
}

fn delete_cache_bytes(
    documents: &mut DocumentMutations<'_, '_, '_>,
    metadata: &PrefixCacheMetadata,
) {
    // Invalid metadata must never create an unbounded cleanup loop.
    let chunks = metadata
        .state_bytes
        .min(MAX_PERSISTENT_ENTRY_BYTES)
        .div_ceil(CACHE_BLOB_CHUNK_BYTES)
        .max(1);
    for index in 0..chunks {
        documents.delete(&blob_chunk_namespace(&metadata.id, index));
    }
}

#[cfg(test)]
fn read_cache_bytes(
    store: &RuntimeStore,
    metadata: &PrefixCacheMetadata,
) -> Result<Option<Vec<u8>>> {
    if !persistent_entry_fits(metadata.state_bytes) {
        return Ok(None);
    }
    store.read_documents(|snapshot| read_cache_bytes_from_snapshot(snapshot, metadata))
}

fn read_cache_bytes_from_snapshot(
    snapshot: &DocumentSnapshot<'_, '_, '_>,
    metadata: &PrefixCacheMetadata,
) -> Result<Option<Vec<u8>>> {
    if !persistent_entry_fits(metadata.state_bytes) {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(metadata.state_bytes)?;
    for index in 0..metadata.state_bytes.div_ceil(CACHE_BLOB_CHUNK_BYTES) {
        let Some(chunk) = snapshot.get_bytes(&blob_chunk_namespace(&metadata.id, index))? else {
            return Ok(None);
        };
        let expected = (metadata.state_bytes - bytes.len()).min(CACHE_BLOB_CHUNK_BYTES);
        if chunk.len() != expected {
            return Ok(None);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(Some(bytes))
}

fn encrypted_blob_uri(id: &str) -> String {
    format!("encrypted://{}", blob_namespace(id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation_store::{Conversation, ConversationDb};
    use llama_native_cache::ValidatedPrefixCacheValue;
    use std::sync::{Arc, Barrier};
    use std::thread;
    use std::time::Duration;

    static STAGED_CHECKPOINT_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn cancelled_startup_does_not_open_storage_or_admit_a_model() {
        let scope = crate::OperationScope::detached();
        scope.request_cancellation();
        let error = prepare_startup_voice_caches(&scope, |_| {
            panic!("cancelled startup must not begin a preparation target");
        })
        .expect_err("cancelled scope must reject preparation");
        assert_eq!(error.to_string(), "Voice setup was cancelled");
    }

    #[derive(Debug, Deserialize)]
    struct W1CacheCorruptionFixture {
        schema: String,
        fixture_key_hex: String,
        native_prefix_namespace: String,
        authoritative_namespace: String,
        authoritative_conversation: Conversation,
        tampered_ciphertext_hex: String,
        native_prefix_disposition: String,
    }

    fn test_cache_value(id: &str, model: &str) -> PrefixCacheValue {
        let fingerprint = CacheFingerprint {
            prompt_form: PromptForm::Chat,
            prompt_token_policy: PromptTokenPolicy::ChatTemplate,
            model_sha256: model.to_string(),
            binding_version: "binding".to_string(),
            build_id: "build".to_string(),
            tokenizer_sha256: "tokenizer".to_string(),
            chat_template_sha256: "template".to_string(),
            multimodal_projector_sha256: None,
            lora_adapters_sha256: Vec::new(),
            context_tokens: 1024,
            batch_tokens: 128,
            max_sequences: 1,
            device: "cpu".to_string(),
            rope_config_sha256: "rope".to_string(),
            kv_layout_sha256: "kv".to_string(),
        };
        PrefixCacheValue {
            metadata: PrefixCacheMetadata::new(
                id,
                CacheTier::MemoryLru,
                fingerprint,
                vec![1],
                1,
                1,
            ),
            sequence: SequenceStateBlob {
                storage_proof: None,
                sequence_id: 0,
                token_count: 1,
                bytes: (vec![1]).into(),
                token_ids: vec![1],
                cells: Vec::new(),
            },
        }
    }

    fn authenticated_session_value(id: &str) -> PrefixCacheValue {
        let mut value = test_cache_value(id, "fingerprint");
        value.metadata.tier = CacheTier::SessionPersistent;
        let mut bytes = b"native-seq-v3\0\0\0".to_vec();
        bytes.extend_from_slice(&[7; 64]);
        value.sequence.bytes = bytes.into();
        value.metadata.state_bytes = value.sequence.bytes.len();
        value.bind_reconstruction();
        value
    }

    fn staged_checkpoint_fixture(
        store: &RuntimeStore,
        id: &str,
        bytes: usize,
    ) -> Result<SessionCheckpointProgress> {
        use crate::conversation_store::{
            Conversation, ConversationDb, ConversationExecutionProfile, ConversationKind,
        };
        let conversation = Conversation {
            id: "staged-conversation".into(),
            title: "fixture".into(),
            created_at: "fixture".into(),
            updated_at: "fixture".into(),
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
            inference_window: None,
        };
        let mut db = ConversationDb::default();
        db.conversations.push(conversation.clone());
        store.put(crate::conversation_store::CONVERSATIONS_NAMESPACE, &db)?;
        let mut value = authenticated_session_value(id);
        value.metadata.owner_id = Some(conversation.id.clone());
        let mut payload = value.sequence.bytes.as_ref().clone();
        payload.resize(bytes.max(payload.len()), 7);
        value.sequence.bytes = payload.into();
        value.metadata.state_bytes = value.sequence.bytes.len();
        value.bind_reconstruction();
        let captured_memory_generation = memory_generation(Some(&conversation.id))?;
        Ok(SessionCheckpointProgress {
            conversation_id: conversation.id.clone(),
            request_id: id.into(),
            store: store.clone(),
            value,
            authority: SessionConversationAuthority::from_conversation(&conversation)?,
            memory_generation: captured_memory_generation,
            next_chunk: 0,
            registered: false,
            max_part_ms: 0,
            snapshot_ms: 0,
            write_started: Instant::now(),
        })
    }

    fn advance_staged_checkpoint_after_lease_contention(
        saved: &mut SessionCheckpointProgress,
        admitted: &dyn Fn() -> bool,
    ) -> Result<CheckpointAdvance> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let result = advance_session_checkpoint(saved, admitted)?;
            if !matches!(result, CheckpointAdvance::Deferred) {
                return Ok(result);
            }
            anyhow::ensure!(
                Instant::now() < deadline,
                "checkpoint publication lease remained unavailable"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn try_persona_cache_lease_after_contention(
        store: &RuntimeStore,
        owner_id: &str,
    ) -> Result<Option<crate::personas::PersonaCacheOwnerLease>> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(lease) =
                crate::personas::try_acquire_persona_cache_owner_lease(store, owner_id)?
            {
                return Ok(Some(lease));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn staged_checkpoint_resumes_parts_and_atomically_publishes_after_preemption() -> Result<()> {
        let _serial = STAGED_CHECKPOINT_TEST_LOCK
            .lock()
            .expect("serialize staged checkpoint cache generation");
        let directory = tempfile::tempdir()?;
        let store = RuntimeStore::open_with_key(directory.path(), [81; 32])?;
        let previous = staged_checkpoint_fixture(&store, "old-complete", 128)?;
        assert!(persist_value_to_store(
            &store,
            &previous.value,
            8,
            64 * 1024 * 1024
        )?);
        let mut next =
            staged_checkpoint_fixture(&store, "new-staged", CACHE_BLOB_CHUNK_BYTES + 128)?;
        let until_first_commit = || {
            store
                .get::<CacheStagingJournal>(KV_CACHE_STAGING_NAMESPACE)
                .unwrap()
                .and_then(|journal| journal.active)
                .is_none_or(|stage| stage.written == 0)
        };
        assert!(matches!(
            advance_session_checkpoint(&mut next, &until_first_commit)?,
            CheckpointAdvance::Deferred
        ));
        assert_eq!(
            next.next_chunk, 1,
            "first committed encrypted part must survive yielding"
        );
        let reopened = RuntimeStore::open_with_key(directory.path(), [81; 32])?;
        let db = reopened.get::<KvCacheDb>(KV_CACHE_NAMESPACE)?.unwrap();
        assert_eq!(db.entries.len(), 1);
        assert_eq!(
            db.entries[0].id, previous.value.metadata.id,
            "partial state must never be advertised"
        );
        assert_eq!(
            read_cache_bytes(&reopened, &previous.value.metadata)?,
            Some(previous.value.sequence.bytes.as_ref().clone())
        );
        assert_eq!(
            reopened
                .get_bytes(&blob_chunk_namespace("new-staged", 0))?
                .unwrap()
                .len(),
            CACHE_BLOB_CHUNK_BYTES
        );
        next.store = reopened.clone();
        assert!(matches!(
            advance_staged_checkpoint_after_lease_contention(&mut next, &|| true)?,
            CheckpointAdvance::Published
        ));
        assert_eq!(next.next_chunk, 2);
        let db = reopened.get::<KvCacheDb>(KV_CACHE_NAMESPACE)?.unwrap();
        assert_eq!(db.entries.len(), 1);
        assert_eq!(db.entries[0].id, "new-staged");
        assert_eq!(
            read_cache_bytes(&reopened, &db.entries[0])?,
            Some(next.value.sequence.bytes.as_ref().clone())
        );
        reclaim_staged_cache_garbage(&reopened, &|| true)?;
        assert!(
            reopened
                .get_bytes(&blob_namespace("old-complete"))?
                .is_none()
        );
        assert!(read_cache_bytes(&reopened, &db.entries[0])?.is_some());
        Ok(())
    }

    #[test]
    fn yielded_checkpoint_holds_no_publication_lock_and_clear_fences_it() -> Result<()> {
        let _serial = STAGED_CHECKPOINT_TEST_LOCK
            .lock()
            .expect("serialize staged checkpoint cache generation");
        let directory = tempfile::tempdir()?;
        let store = RuntimeStore::open_with_key(directory.path(), [84; 32])?;
        let mut next =
            staged_checkpoint_fixture(&store, "clear-staged", CACHE_BLOB_CHUNK_BYTES + 128)?;
        let until_first_commit = || {
            store
                .get::<CacheStagingJournal>(KV_CACHE_STAGING_NAMESPACE)
                .unwrap()
                .and_then(|journal| journal.active)
                .is_none_or(|stage| stage.written == 0)
        };
        assert!(matches!(
            advance_session_checkpoint(&mut next, &until_first_commit)?,
            CheckpointAdvance::Deferred
        ));
        let foreground = try_persona_cache_lease_after_contention(&store, &next.conversation_id)?;
        assert!(
            foreground.is_some(),
            "yielded cache must not hold a lock needed by foreground work"
        );
        drop(foreground);
        clear_checkpoint_store(&store)?;
        assert!(matches!(
            advance_staged_checkpoint_after_lease_contention(&mut next, &|| true)?,
            CheckpointAdvance::Obsolete
        ));
        assert!(
            store
                .get::<KvCacheDb>(KV_CACHE_NAMESPACE)?
                .unwrap()
                .entries
                .is_empty()
        );
        reclaim_staged_cache_garbage(&store, &|| true)?;
        assert!(store.get_bytes(&blob_namespace("clear-staged"))?.is_none());
        Ok(())
    }

    #[test]
    fn abandoned_staging_is_fenced_and_reclaimed_without_replacing_a_complete_cache() -> Result<()>
    {
        let _serial = STAGED_CHECKPOINT_TEST_LOCK
            .lock()
            .expect("serialize staged checkpoint cache generation");
        let directory = tempfile::tempdir()?;
        let store = RuntimeStore::open_with_key(directory.path(), [82; 32])?;
        let previous = staged_checkpoint_fixture(&store, "durable", 128)?;
        assert!(persist_value_to_store(
            &store,
            &previous.value,
            8,
            64 * 1024 * 1024
        )?);
        let mut interrupted =
            staged_checkpoint_fixture(&store, "abandoned", CACHE_BLOB_CHUNK_BYTES + 128)?;
        let until_first_commit = || {
            store
                .get::<CacheStagingJournal>(KV_CACHE_STAGING_NAMESPACE)
                .unwrap()
                .and_then(|journal| journal.active)
                .is_none_or(|stage| stage.written == 0)
        };
        assert!(matches!(
            advance_session_checkpoint(&mut interrupted, &until_first_commit)?,
            CheckpointAdvance::Deferred
        ));
        let mut replacement = staged_checkpoint_fixture(&store, "replacement", 128)?;
        assert!(matches!(
            advance_staged_checkpoint_after_lease_contention(&mut replacement, &|| true)?,
            CheckpointAdvance::Published
        ));
        assert!(store.get_bytes(&blob_namespace("abandoned"))?.is_none());
        assert!(matches!(
            advance_staged_checkpoint_after_lease_contention(&mut interrupted, &|| true)?,
            CheckpointAdvance::Obsolete
        ));
        let db = store.get::<KvCacheDb>(KV_CACHE_NAMESPACE)?.unwrap();
        assert_eq!(db.entries[0].id, "replacement");
        assert!(read_cache_bytes(&store, &db.entries[0])?.is_some());
        Ok(())
    }

    #[test]
    fn staged_checkpoint_rechecks_conversation_authority_at_publication() -> Result<()> {
        let _serial = STAGED_CHECKPOINT_TEST_LOCK
            .lock()
            .expect("serialize staged checkpoint cache generation");
        let directory = tempfile::tempdir()?;
        let store = RuntimeStore::open_with_key(directory.path(), [83; 32])?;
        let previous = staged_checkpoint_fixture(&store, "durable-authority", 128)?;
        assert!(persist_value_to_store(
            &store,
            &previous.value,
            8,
            64 * 1024 * 1024
        )?);
        let mut next =
            staged_checkpoint_fixture(&store, "stale-source", CACHE_BLOB_CHUNK_BYTES + 128)?;
        let until_first_commit = || {
            store
                .get::<CacheStagingJournal>(KV_CACHE_STAGING_NAMESPACE)
                .unwrap()
                .and_then(|journal| journal.active)
                .is_none_or(|stage| stage.written == 0)
        };
        assert!(matches!(
            advance_session_checkpoint(&mut next, &until_first_commit)?,
            CheckpointAdvance::Deferred
        ));
        store.put(
            crate::conversation_store::CONVERSATIONS_NAMESPACE,
            &crate::conversation_store::ConversationDb::default(),
        )?;
        assert!(matches!(
            advance_staged_checkpoint_after_lease_contention(&mut next, &|| true)?,
            CheckpointAdvance::Obsolete
        ));
        let db = store.get::<KvCacheDb>(KV_CACHE_NAMESPACE)?.unwrap();
        assert_eq!(db.entries[0].id, "durable-authority");
        assert!(read_cache_bytes(&store, &db.entries[0])?.is_some());
        Ok(())
    }

    #[test]
    fn staged_checkpoint_defers_while_persona_authority_is_held() -> Result<()> {
        let _serial = STAGED_CHECKPOINT_TEST_LOCK
            .lock()
            .expect("serialize staged checkpoint cache generation");
        let directory = tempfile::tempdir()?;
        let store = RuntimeStore::open_with_key(directory.path(), [85; 32])?;
        let mut next =
            staged_checkpoint_fixture(&store, "lease-contention", CACHE_BLOB_CHUNK_BYTES + 128)?;
        let held =
            crate::personas::acquire_persona_cache_owner_lease(&store, &next.conversation_id)?
                .expect("active Persona cache owner lease");
        assert!(matches!(
            advance_session_checkpoint(&mut next, &|| true)?,
            CheckpointAdvance::Deferred
        ));
        drop(held);
        assert!(matches!(
            advance_staged_checkpoint_after_lease_contention(&mut next, &|| true)?,
            CheckpointAdvance::Published
        ));
        let db = store.get::<KvCacheDb>(KV_CACHE_NAMESPACE)?.unwrap();
        assert_eq!(db.entries[0].id, "lease-contention");
        Ok(())
    }

    #[test]
    fn session_checkpoint_selection_is_latest_ready_exact_owner_and_profile() {
        let base = test_cache_value("base", "model-a").metadata;
        let fingerprint = base.fingerprint.clone();
        let mut older = base.clone();
        older.tier = CacheTier::SessionPersistent;
        older.owner_id = Some("conversation-a".to_owned());
        older.last_used_at_ms = 2;
        older.created_at_ms = 1;
        let mut latest = older.clone();
        latest.id = "latest".to_owned();
        latest.last_used_at_ms = 3;
        latest.created_at_ms = 2;
        let mut foreign = latest.clone();
        foreign.id = "foreign".to_owned();
        foreign.owner_id = Some("conversation-b".to_owned());
        foreign.last_used_at_ms = u128::MAX;
        let mut wrong_profile = latest.clone();
        wrong_profile.id = "wrong-profile".to_owned();
        wrong_profile.fingerprint.model_sha256 = "model-b".to_owned();
        wrong_profile.last_used_at_ms = u128::MAX;
        let mut invalid = latest.clone();
        invalid.id = "invalid".to_owned();
        invalid.state = CacheEntryState::Invalidated;
        invalid.last_used_at_ms = u128::MAX;

        let entries = [older, latest.clone(), foreign, wrong_profile, invalid];
        let selected = latest_session_checkpoint(&entries, &fingerprint, "conversation-a");
        assert_eq!(selected.map(|entry| entry.id.as_str()), Some("latest"));
    }

    #[test]
    fn cache_authority_includes_mixed_cells_and_reconstruction() {
        let mut value = test_cache_value("authority", "model");
        value.bind_reconstruction();
        let metadata = value.metadata;

        let mut changed_cells = metadata.clone();
        changed_cells.cells = vec![SequenceStateCell::AudioFrame { digest: [7; 32] }];
        assert!(!same_cache_authority(&metadata, &changed_cells));

        let mut changed_reconstruction = metadata.clone();
        changed_reconstruction
            .reconstruction
            .as_mut()
            .expect("test value has native reconstruction")
            .header
            .push(0);
        assert!(!same_cache_authority(&metadata, &changed_reconstruction));
        assert!(same_cache_authority(&metadata, &metadata.clone()));
    }

    #[test]
    fn waiting_foreground_write_preempts_checkpoint_and_preserves_previous_state() -> Result<()> {
        let root = std::env::temp_dir().join(format!("mom-cache-navigation-{}", Uuid::new_v4()));
        let store = RuntimeStore::open_with_key(&root, [68; 32])?;
        let previous = test_cache_value("previous", "model");
        assert!(persist_value_to_store(
            &store,
            &previous,
            4,
            CACHE_BLOB_CHUNK_BYTES * 2
        )?);
        let bytes = vec![7; CACHE_BLOB_CHUNK_BYTES + 1];
        let (start, ready) = std::sync::mpsc::channel();
        let foreground_store = store.clone();
        let writer = std::thread::spawn(move || -> Result<()> {
            ready.recv_timeout(Duration::from_secs(5))?;
            foreground_store.put("navigation-test-selection", &"second-chat")
        });
        let checks = std::cell::Cell::new(0);
        let interrupted: Result<()> = store.mutate_documents(
            KV_CACHE_NAMESPACE,
            KvCacheDb::default,
            |db: &mut KvCacheDb, documents| {
                delete_cache_bytes(documents, &previous.metadata);
                db.entries.clear();
                write_cache_bytes_with_admission(documents, "next", &bytes, &|| {
                    let count = checks.get() + 1;
                    checks.set(count);
                    if count == 1 {
                        return true;
                    }
                    start
                        .send(())
                        .expect("foreground writer is waiting for admission");
                    let deadline = Instant::now() + Duration::from_secs(2);
                    while !crate::store::store_write_pending() && Instant::now() < deadline {
                        std::thread::yield_now();
                    }
                    assert!(
                        crate::store::store_write_pending(),
                        "foreground write must reach the real coordinator"
                    );
                    !crate::store::store_write_pending()
                })
            },
        );
        writer.join().expect("foreground writer panicked")?;
        assert!(
            interrupted
                .expect_err("checkpoint must yield")
                .is::<CachePersistencePreempted>()
        );
        assert_eq!(checks.get(), 2);
        assert_eq!(
            store.get::<String>("navigation-test-selection")?.as_deref(),
            Some("second-chat")
        );
        assert_eq!(
            read_cache_bytes(&store, &previous.metadata)?.as_deref(),
            Some(previous.sequence.bytes.as_slice())
        );
        assert!(store.get_bytes(&blob_chunk_namespace("next", 0))?.is_none());
        assert!(store.get_bytes(&blob_chunk_namespace("next", 1))?.is_none());
        assert_eq!(
            store
                .get::<KvCacheDb>(KV_CACHE_NAMESPACE)?
                .expect("checkpoint cache should remain stored")
                .entries[0]
                .id,
            "previous"
        );
        Ok(())
    }

    #[test]
    fn call_preempts_chunked_checkpoint_without_replacing_durable_state() -> Result<()> {
        let root = std::env::temp_dir().join(format!("mom-cache-preempt-{}", Uuid::new_v4()));
        let store = RuntimeStore::open_with_key(&root, [69; 32])?;
        let previous = test_cache_value("previous", "model");
        assert!(persist_value_to_store(
            &store,
            &previous,
            4,
            CACHE_BLOB_CHUNK_BYTES * 2
        )?);
        let mut next = test_cache_value("next", "model");
        next.sequence.bytes = vec![7; CACHE_BLOB_CHUNK_BYTES + 23].into();
        next.metadata.state_bytes = next.sequence.bytes.len();
        let checks = std::cell::Cell::new(0);
        let interrupted: Result<()> = store.mutate_documents(
            KV_CACHE_NAMESPACE,
            KvCacheDb::default,
            |db: &mut KvCacheDb, documents| {
                delete_cache_bytes(documents, &previous.metadata);
                db.entries.clear();
                write_cache_bytes_with_admission(
                    documents,
                    &next.metadata.id,
                    &next.sequence.bytes,
                    &|| {
                        let count = checks.get() + 1;
                        checks.set(count);
                        // Part zero is staged; the call arrives before part one.
                        count == 1
                    },
                )?;
                db.entries.push(next.metadata.clone());
                Ok(())
            },
        );
        assert!(
            interrupted
                .expect_err("the foreground call should preempt the second chunk")
                .is::<CachePersistencePreempted>()
        );
        assert_eq!(checks.get(), 2);
        assert!(!persist_value_to_store_with_admission(
            &store,
            &next,
            4,
            CACHE_BLOB_CHUNK_BYTES * 2,
            None,
            &|| false,
        )?);
        assert_eq!(
            read_cache_bytes(&store, &previous.metadata)?.as_deref(),
            Some(previous.sequence.bytes.as_slice())
        );
        assert!(store.get_bytes(&blob_chunk_namespace("next", 0))?.is_none());
        assert!(store.get_bytes(&blob_chunk_namespace("next", 1))?.is_none());
        let db = store
            .get::<KvCacheDb>(KV_CACHE_NAMESPACE)?
            .expect("the previous durable cache database remains present");
        assert_eq!(db.entries.len(), 1);
        assert_eq!(db.entries[0].id, "previous");
        // The same snapshot remains retryable after the call; no state reset.
        assert!(persist_value_to_store(
            &store,
            &next,
            4,
            CACHE_BLOB_CHUNK_BYTES * 2
        )?);
        assert_eq!(
            read_cache_bytes(&store, &next.metadata)?.as_deref(),
            Some(next.sequence.bytes.as_slice())
        );
        Ok(())
    }

    #[test]
    fn encrypted_cache_parts_round_trip_and_replacement_removes_the_tail() -> Result<()> {
        let root = std::env::temp_dir().join(format!("mom-cache-parts-{}", Uuid::new_v4()));
        let store = RuntimeStore::open_with_key(&root, [67; 32])?;
        let mut value = test_cache_value("chunked", "model");
        let mut bytes = vec![7; CACHE_BLOB_CHUNK_BYTES + 23];
        bytes[CACHE_BLOB_CHUNK_BYTES] = 9;
        value.sequence.bytes = bytes.clone().into();
        value.metadata.state_bytes = bytes.len();
        assert!(persist_value_to_store(&store, &value, 4, bytes.len() * 2)?);
        assert_eq!(read_cache_bytes(&store, &value.metadata)?, Some(bytes));
        assert_eq!(
            store
                .get_bytes(&blob_chunk_namespace("chunked", 1))?
                .ok_or_else(|| anyhow!("second encrypted cache part is missing"))?
                .len(),
            23
        );
        // A missing part is an incomplete cache, never a partially accepted state.
        store.mutate_documents(KV_CACHE_NAMESPACE, KvCacheDb::default, |_, documents| {
            documents.delete(&blob_chunk_namespace("chunked", 1));
            Ok(())
        })?;
        assert!(read_cache_bytes(&store, &value.metadata)?.is_none());
        assert!(persist_value_to_store(
            &store,
            &value,
            4,
            value.metadata.state_bytes * 2
        )?);
        value.sequence.bytes = vec![3; 9].into();
        value.metadata.state_bytes = 9;
        assert!(persist_value_to_store(
            &store,
            &value,
            4,
            CACHE_BLOB_CHUNK_BYTES * 2
        )?);
        assert_eq!(read_cache_bytes(&store, &value.metadata)?, Some(vec![3; 9]));
        assert!(
            store
                .get_bytes(&blob_chunk_namespace("chunked", 1))?
                .is_none()
        );
        drop(store);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn built_in_cache_owner_has_a_stable_nonempty_prompt() -> Result<()> {
        assert_eq!(cache_prompt(None)?, BUILTIN_CACHE_PROMPT);
        assert_eq!(
            cache_prompt(Some(BUILTIN_CACHE_OWNER))?,
            BUILTIN_CACHE_PROMPT
        );
        Ok(())
    }

    #[test]
    fn persona_cache_uses_the_same_system_prefix_as_live_chat() -> Result<()> {
        let settings = Settings::defaults_for_data_dir(PathBuf::from("/tmp/persona-cache-test"));
        let persona: Conversation = serde_json::from_value(serde_json::json!({
            "id": "persona-example",
            "title": "Example",
            "created_at": "1",
            "updated_at": "1",
            "kind": "persona_template",
            "execution_profile": {"system_message": "Speak as Example."},
            "selected_model_path": null,
            "messages": []
        }))?;
        let cached = stable_persona_prompt_messages(&settings, &persona)?;
        let live = crate::chat::build_native_messages(
            "Speak as Example.",
            "",
            &[],
            "Hello",
            false,
            &HashMap::new(),
            "",
        );
        assert_eq!(cached, live[..1]);
        Ok(())
    }

    fn match_with(id: &str, tier: CacheTier, matched_tokens: usize) -> CacheMatch {
        CacheMatch {
            id: id.to_string(),
            tier,
            matched_tokens,
            exact: false,
        }
    }

    #[test]
    fn longest_cache_wins_across_memory_and_persistent_tiers() {
        let selected = choose_match(
            Some(match_with("memory", CacheTier::MemoryLru, 12)),
            Some(match_with("persona", CacheTier::PersonaPack, 19)),
        )
        .expect("selected match");
        assert_eq!(selected.id, "persona");
    }

    #[test]
    fn memory_wins_equal_length_tie() {
        let selected = choose_match(
            Some(match_with("memory", CacheTier::MemoryLru, 19)),
            Some(match_with("persona", CacheTier::PersonaPack, 19)),
        )
        .expect("selected match");
        assert_eq!(selected.id, "memory");
    }

    #[test]
    fn memory_cache_budget_is_global_across_model_fingerprints() {
        let mut state = memory_cache().lock().expect("memory cache lock");
        state.clear();
        let values = [
            test_cache_value("model-a", "fingerprint-a"),
            test_cache_value("model-b", "fingerprint-b"),
        ];
        let expected_bytes = values
            .iter()
            .cloned()
            .map(|value| {
                ValidatedPrefixCacheValue::new(value)
                    .expect("valid cache fixture")
                    .memory_bytes()
            })
            .sum::<usize>();
        for value in values {
            assert!(matches!(
                state.cache.insert(value),
                llama_native_cache::CacheInsertOutcome::Stored {
                    memory_resident: true,
                    ..
                }
            ));
        }
        assert_eq!(state.cache.len(), 2);
        assert_eq!(state.cache.used_bytes(), expected_bytes);
        assert_eq!(state.cache.capacity_bytes(), DEFAULT_MEMORY_CACHE_BYTES);
        state.clear();
    }

    #[test]
    fn owner_generation_rejects_promotion_after_exact_invalidation() {
        let mut state = MemoryCacheState::new();
        let mut value = test_cache_value("stale-promotion", "fingerprint");
        value.metadata = value.metadata.with_owner("removed-persona");
        let captured = state.generation(Some("removed-persona"));

        assert_eq!(state.invalidate_owner("removed-persona"), 0);
        assert!(!state.promote_if_current(value, captured));
        assert_eq!(state.cache.len(), 0);
    }

    #[test]
    fn memory_only_lookup_uses_longest_ram_match_and_never_falls_back() {
        const OWNER: &str = "voice-memory-only-owner";
        let cache = Mutex::new(MemoryCacheState::new());
        let fingerprint = test_cache_value("unused", "fingerprint")
            .metadata
            .fingerprint;
        let make_value = |id: &str, token_ids: Vec<i32>| {
            let metadata = PrefixCacheMetadata::new(
                id,
                CacheTier::MemoryLru,
                fingerprint.clone(),
                token_ids.clone(),
                1,
                1,
            )
            .with_owner(OWNER.to_string());
            PrefixCacheValue {
                metadata,
                sequence: SequenceStateBlob {
                    storage_proof: None,
                    sequence_id: 0,
                    token_count: token_ids.len(),
                    bytes: vec![1].into(),
                    token_ids,
                    cells: Vec::new(),
                },
            }
        };
        let short = make_value("short", vec![1, 2]);
        let long = make_value("long", vec![1, 2, 3]);
        {
            let mut state = cache.lock().expect("memory cache lock");
            let generation = state.generation(Some(OWNER));
            assert!(state.promote_if_current(short, generation));
            assert!(state.promote_if_current(long, generation));
        }

        let prompt = [1, 2, 3, 4];
        let selected = memory_cached_prefix(&cache, OWNER, &fingerprint, &prompt, 0, 10)
            .expect("the longest compatible RAM prefix should be selected");
        assert_eq!(selected.metadata.id, "long");
        assert!(
            memory_cached_prefix(&cache, OWNER, &fingerprint, &prompt, 3, 11).is_none(),
            "a longer engine-resident prefix must remain selected"
        );
        assert!(
            memory_cached_prefix(&cache, "other-owner", &fingerprint, &prompt, 0, 12).is_none(),
            "a prefix owned by another persona must not be reused"
        );

        {
            let mut state = cache.lock().expect("memory cache lock");
            state.invalidate_owner(OWNER);
        }
        assert!(
            memory_cached_prefix(&cache, OWNER, &fingerprint, &prompt, 0, 13).is_none(),
            "owner invalidation must remove RAM matches before any disk lookup"
        );
        let empty = Mutex::new(MemoryCacheState::new());
        assert!(
            memory_cached_prefix(&empty, OWNER, &fingerprint, &prompt, 0, 14).is_none(),
            "a memory-only miss returns no candidate; it has no persistent fallback"
        );
    }

    #[test]
    fn resident_text_or_media_prefix_avoids_snapshot_restore_only_for_exact_tokens() {
        let prompt = [1, 2, 3, 4];
        let snapshot = [1, 2, 3];
        assert!(resident_prefix_covers_snapshot(3, 0, &prompt, &snapshot));
        assert!(resident_prefix_covers_snapshot(0, 3, &prompt, &snapshot));
        assert!(!resident_prefix_covers_snapshot(2, 2, &prompt, &snapshot));
        assert!(!resident_prefix_covers_snapshot(3, 0, &prompt, &[1, 2, 9]));
        assert!(!resident_prefix_covers_snapshot(4, 0, &prompt, &[]));
    }

    #[test]
    fn owner_invalidation_after_persistence_rejects_memory_promotion() -> Result<()> {
        let suffix = Uuid::new_v4();
        let data_dir =
            std::env::temp_dir().join(format!("mom-llama-cache-stale-promotion-{suffix}"));
        let store = RuntimeStore::open_with_key(&data_dir, [31_u8; 32])?;
        let owner_id = format!("persona-{suffix}");
        let cache_id = format!("cache-{suffix}");
        let mut value = test_cache_value(&cache_id, "fingerprint");
        value.metadata = value.metadata.with_owner(owner_id.clone());

        let promoted = persist_value_with_promotion_hook(&store, &value, || {
            invalidate_persona_memory_cache(&owner_id)
                .expect("exact owner invalidation must remain available");
        })?;

        assert!(!promoted);
        assert!(
            memory_cache()
                .lock()
                .expect("memory cache lock")
                .cache
                .get(&cache_id, now_ms())
                .is_none()
        );
        let metadata = store
            .get::<KvCacheDb>(KV_CACHE_NAMESPACE)?
            .ok_or_else(|| anyhow!("cache metadata missing after rejected promotion"))?;
        assert_eq!(
            metadata
                .entries
                .iter()
                .find(|entry| entry.id == cache_id)
                .map(|entry| entry.state),
            Some(CacheEntryState::Invalidated)
        );
        assert_eq!(store.get_bytes(&blob_namespace(&cache_id))?, None);
        Ok(())
    }

    #[test]
    fn same_id_owned_ownerless_replacement_invalidates_selected_memory_authority() {
        const OWNER: &str = "persona-memory-owner-race";
        let cache = Arc::new(Mutex::new(MemoryCacheState::new()));
        let prompt_token_ids = vec![1, 2];
        let ownerless = test_cache_value("same-id-owner-race", "fingerprint");
        let fingerprint = ownerless.metadata.fingerprint.clone();
        {
            let mut state = cache.lock().expect("memory cache lock");
            let generation = state.generation(None);
            assert!(state.promote_if_current(ownerless.clone(), generation));
        }

        let (selected_ownerless, ownerless_generation) = {
            let state = cache.lock().expect("memory cache lock");
            (
                state
                    .cache
                    .best_match(&fingerprint, &prompt_token_ids)
                    .expect("ownerless selection"),
                state.generation(None),
            )
        };
        let before_ownerless_fetch = Arc::new(Barrier::new(2));
        let release_ownerless_fetch = Arc::new(Barrier::new(2));
        let worker_cache = Arc::clone(&cache);
        let worker_before = Arc::clone(&before_ownerless_fetch);
        let worker_release = Arc::clone(&release_ownerless_fetch);
        let worker_fingerprint = fingerprint.clone();
        let worker_prompt = prompt_token_ids.clone();
        let ownerless_worker = thread::spawn(move || {
            worker_before.wait();
            worker_release.wait();
            selected_memory_value(
                worker_cache.as_ref(),
                &selected_ownerless,
                ownerless_generation,
                None,
                &worker_fingerprint,
                &worker_prompt,
                now_ms(),
            )
        });

        before_ownerless_fetch.wait();
        let mut owned = ownerless.clone();
        owned.metadata = owned.metadata.with_owner(OWNER);
        {
            let mut state = cache.lock().expect("memory cache lock");
            let generation = state.generation(Some(OWNER));
            assert!(state.promote_if_current(owned.clone(), generation));
        }
        release_ownerless_fetch.wait();
        assert!(
            ownerless_worker
                .join()
                .expect("ownerless fetch worker")
                .is_none(),
            "an owned replacement must not inherit a prior ownerless selection"
        );

        let (selected_owned, owned_generation) = {
            let state = cache.lock().expect("memory cache lock");
            (
                state
                    .cache
                    .best_match_for_owner(&fingerprint, &prompt_token_ids, OWNER)
                    .expect("owned selection"),
                state.generation(Some(OWNER)),
            )
        };
        let before_owned_fetch = Arc::new(Barrier::new(2));
        let release_owned_fetch = Arc::new(Barrier::new(2));
        let worker_cache = Arc::clone(&cache);
        let worker_before = Arc::clone(&before_owned_fetch);
        let worker_release = Arc::clone(&release_owned_fetch);
        let worker_fingerprint = fingerprint.clone();
        let worker_prompt = prompt_token_ids.clone();
        let owned_worker = thread::spawn(move || {
            worker_before.wait();
            worker_release.wait();
            selected_memory_value(
                worker_cache.as_ref(),
                &selected_owned,
                owned_generation,
                Some(OWNER),
                &worker_fingerprint,
                &worker_prompt,
                now_ms(),
            )
        });

        before_owned_fetch.wait();
        {
            let mut state = cache.lock().expect("memory cache lock");
            let generation = state.generation(None);
            assert!(state.promote_if_current(ownerless, generation));
        }
        release_owned_fetch.wait();
        assert!(
            owned_worker.join().expect("owned fetch worker").is_none(),
            "an ownerless replacement must not inherit a prior owned selection"
        );
    }

    #[test]
    fn persistent_cache_budget_evicts_invalid_and_least_recent_entries() {
        let metadata = |id: &str, bytes: usize, last_used: u128| {
            let mut metadata = test_cache_value(id, "fingerprint").metadata;
            metadata.tier = CacheTier::SessionPersistent;
            metadata.state_bytes = bytes;
            metadata.created_at_ms = last_used;
            metadata.last_used_at_ms = last_used;
            metadata
        };
        let mut invalid = metadata("invalid", 4, 0);
        invalid.state = CacheEntryState::Invalidated;
        let entries = vec![
            invalid,
            metadata("oldest", 4, 1),
            metadata("recent", 4, 2),
            metadata("new", 4, 3),
        ];
        let count_evicted = persistent_eviction_ids(&entries, "new", 2, 100);
        assert_eq!(count_evicted, vec!["invalid", "oldest"]);
        let byte_evicted = persistent_eviction_ids(&entries, "new", 10, 8);
        assert_eq!(byte_evicted, vec!["invalid", "oldest"]);
    }

    #[test]
    fn persistent_budget_holds_multiple_full_sessions_with_per_entry_cap() {
        let entries = (0..8)
            .map(|index| {
                let mut metadata = test_cache_value(&format!("session-{index}"), "model").metadata;
                metadata.tier = CacheTier::SessionPersistent;
                metadata.state_bytes = MAX_PERSISTENT_ENTRY_BYTES;
                metadata
            })
            .collect::<Vec<_>>();

        assert!(persistent_entry_fits(MAX_PERSISTENT_ENTRY_BYTES));
        assert!(!persistent_entry_fits(MAX_PERSISTENT_ENTRY_BYTES + 1));
        const {
            assert!(DEFAULT_PERSISTENT_CACHE_BYTES >= 8 * MAX_PERSISTENT_ENTRY_BYTES);
        }
        assert!(
            persistent_eviction_ids(
                &entries,
                "session-7",
                DEFAULT_PERSISTENT_CACHE_ENTRIES,
                DEFAULT_PERSISTENT_CACHE_BYTES,
            )
            .is_empty()
        );
    }

    #[test]
    fn native_prefix_corruption_preserves_session_metadata_and_authoritative_conversation()
    -> Result<()> {
        let fixture: W1CacheCorruptionFixture =
            serde_json::from_str(include_str!("../fixtures/compat/cache-corruption-v1.json"))?;
        assert_eq!(
            fixture.schema,
            "mom_llama.w1.disposable_cache_corruption_fixture.v1"
        );
        assert_eq!(fixture.fixture_key_hex, "17".repeat(32));
        assert_eq!(
            fixture.native_prefix_disposition,
            "quarantine_then_cold_miss"
        );
        assert_eq!(fixture.tampered_ciphertext_hex, "00");
        let data_dir =
            std::env::temp_dir().join(format!("mom-llama-cache-corruption-{}", crate::now_ms()));
        let store = RuntimeStore::open_with_key(&data_dir, [17_u8; 32])?;
        let session_value = authenticated_session_value("preserved-v3-session");
        assert!(persist_value_to_store(
            &store,
            &session_value,
            4,
            1024 * 1024
        )?);
        let session_metadata = session_value.metadata.clone();
        let session_state = Arc::clone(&session_value.sequence.bytes);
        let authoritative = ConversationDb {
            selected_conversation_id: Some(fixture.authoritative_conversation.id.clone()),
            conversations: vec![fixture.authoritative_conversation.clone()],
        };
        store.put(&fixture.authoritative_namespace, &authoritative)?;
        let native_prefix = vec![test_cache_value("native-prefix-fixture", "native-model")];
        store.put(&fixture.native_prefix_namespace, &native_prefix)?;
        let mut native_before = serde_json::to_vec_pretty(&native_prefix)?;
        native_before.push(b'\n');
        assert_eq!(
            native_before,
            include_bytes!("../fixtures/compat/cache-native-prefix-state-v1.json")
        );
        let connection = rusqlite::Connection::open(store.path())?;
        connection.execute(
            "UPDATE encrypted_documents SET ciphertext = X'00' WHERE namespace = ?1",
            [&fixture.native_prefix_namespace],
        )?;

        assert_eq!(
            store
                .get_disposable_cache::<Vec<PrefixCacheValue>>(&fixture.native_prefix_namespace)?,
            None
        );
        assert_eq!(
            store.get::<ConversationDb>(&fixture.authoritative_namespace)?,
            Some(authoritative.clone())
        );
        let session_before_reopen =
            load_persistent_value_or_invalidate_from_store(&store, &session_metadata)?
                .expect("unrelated native-prefix corruption preserves v3 session cache");
        assert_eq!(session_before_reopen.metadata, session_metadata);
        assert_eq!(session_before_reopen.sequence.bytes, session_state);
        assert!(
            session_before_reopen
                .sequence
                .has_authenticated_storage_proof()
        );
        drop(connection);
        drop(store);
        let store = RuntimeStore::open_with_key(&data_dir, [17_u8; 32])?;
        let native_after = store
            .get_disposable_cache::<Vec<PrefixCacheValue>>(&fixture.native_prefix_namespace)?;
        assert_eq!(native_after, None);
        let mut native_after_bytes = serde_json::to_vec_pretty(&native_after)?;
        native_after_bytes.push(b'\n');
        assert_eq!(
            native_after_bytes,
            include_bytes!("../fixtures/compat/cache-native-prefix-after-state-v1.json")
        );
        assert_eq!(
            store.get::<ConversationDb>(&fixture.authoritative_namespace)?,
            Some(authoritative.clone())
        );
        let session_after_reopen =
            load_persistent_value_or_invalidate_from_store(&store, &session_metadata)?
                .expect("v3 session cache survives reopen with native-prefix corruption");
        assert_eq!(session_after_reopen.metadata, session_metadata);
        assert_eq!(session_after_reopen.sequence.bytes, session_state);
        assert!(
            session_after_reopen
                .sequence
                .has_authenticated_storage_proof()
        );
        assert_eq!(
            store.get::<ConversationDb>(&fixture.authoritative_namespace)?,
            Some(authoritative)
        );
        Ok(())
    }

    #[test]
    fn authenticated_session_snapshot_proof_survives_reopen_but_not_serialization() -> Result<()> {
        let data_dir = std::env::temp_dir().join(format!("mom-v3-cache-proof-{}", Uuid::new_v4()));
        let store = RuntimeStore::open_with_key(&data_dir, [31_u8; 32])?;
        let value = authenticated_session_value("v3-proof");
        assert!(persist_value_to_store(&store, &value, 4, 1024 * 1024)?);

        let loaded = load_persistent_value_or_invalidate_from_store(&store, &value.metadata)?
            .expect("AEAD-authenticated session value loads");
        assert!(loaded.sequence.has_authenticated_storage_proof());
        let serialized = serde_json::to_vec(&loaded)?;
        let decoded: PrefixCacheValue = serde_json::from_slice(&serialized)?;
        assert!(!decoded.sequence.has_authenticated_storage_proof());

        drop(store);
        let reopened = RuntimeStore::open_with_key(&data_dir, [31_u8; 32])?;
        let loaded_after_reopen =
            load_persistent_value_or_invalidate_from_store(&reopened, &value.metadata)?
                .expect("authenticated session value remains loadable after reopen");
        assert!(
            loaded_after_reopen
                .sequence
                .has_authenticated_storage_proof()
        );
        std::fs::remove_dir_all(data_dir)?;
        Ok(())
    }

    #[test]
    fn session_snapshot_rejects_forged_metadata_before_minting_proof() -> Result<()> {
        let data_dir =
            std::env::temp_dir().join(format!("mom-v3-cache-forgery-{}", Uuid::new_v4()));
        let store = RuntimeStore::open_with_key(&data_dir, [32_u8; 32])?;
        let value = authenticated_session_value("v3-forgery");
        assert!(persist_value_to_store(&store, &value, 4, 1024 * 1024)?);

        let mut forged = value.metadata.clone();
        forged
            .reconstruction
            .as_mut()
            .expect("v3 reconstruction")
            .sequence_id += 1;
        assert!(load_persistent_value_or_invalidate_from_store(&store, &forged)?.is_none());
        assert_eq!(
            store
                .get::<KvCacheDb>(KV_CACHE_NAMESPACE)?
                .expect("metadata")
                .entries[0]
                .state,
            CacheEntryState::Ready,
            "a stale or forged argument does not invalidate the authenticated current entry"
        );
        let loaded = load_persistent_value_or_invalidate_from_store(&store, &value.metadata)?
            .expect("current authenticated metadata remains usable");
        assert!(loaded.sequence.has_authenticated_storage_proof());
        std::fs::remove_dir_all(data_dir)?;
        Ok(())
    }

    #[test]
    fn missing_or_tampered_session_snapshot_chunk_invalidates_without_proof() -> Result<()> {
        for (id, tamper) in [("v3-missing", false), ("v3-tampered", true)] {
            let data_dir =
                std::env::temp_dir().join(format!("mom-v3-cache-invalid-{id}-{}", Uuid::new_v4()));
            let store = RuntimeStore::open_with_key(&data_dir, [33_u8; 32])?;
            let value = authenticated_session_value(id);
            assert!(persist_value_to_store(&store, &value, 4, 1024 * 1024)?);

            if tamper {
                let connection = rusqlite::Connection::open(store.path())?;
                connection.execute(
                    "UPDATE encrypted_documents SET ciphertext = X'00' WHERE namespace = ?1",
                    [blob_chunk_namespace(id, 0)],
                )?;
                drop(connection);
            } else {
                store.mutate_documents(
                    KV_CACHE_NAMESPACE,
                    KvCacheDb::default,
                    |_, documents| {
                        documents.delete(&blob_chunk_namespace(id, 0));
                        Ok(())
                    },
                )?;
            }

            assert!(
                load_persistent_value_or_invalidate_from_store(&store, &value.metadata)?.is_none()
            );
            assert_eq!(
                store
                    .get::<KvCacheDb>(KV_CACHE_NAMESPACE)?
                    .expect("metadata")
                    .entries[0]
                    .state,
                CacheEntryState::Invalidated
            );
            drop(store);
            std::fs::remove_dir_all(data_dir)?;
        }
        Ok(())
    }

    #[test]
    fn unsupported_legacy_snapshot_is_ignored_without_rewriting() -> Result<()> {
        let data_dir = std::env::temp_dir().join(format!("mom-cache-lifetime-{}", Uuid::new_v4()));
        let store = RuntimeStore::open_with_key(&data_dir, [31_u8; 32])?;
        let mut value = test_cache_value("spill", "fingerprint");
        value.metadata.tier = CacheTier::SessionPersistent;
        let mut legacy_bytes = b"native-seq-v1\0\0\0".to_vec();
        legacy_bytes.extend_from_slice(&[7; 64 * 1024]);
        value.sequence.bytes = legacy_bytes.into();
        value.sequence.sequence_id = 3;
        value.metadata.state_bytes = value.sequence.bytes.len();
        value.bind_reconstruction();
        persist_value_to_store(&store, &value, 4, 1024 * 1024)?;
        let stored_metadata = store
            .get::<KvCacheDb>(KV_CACHE_NAMESPACE)?
            .expect("metadata")
            .entries[0]
            .clone();
        assert!(load_persistent_value_or_invalidate_from_store(&store, &value.metadata)?.is_none());
        assert_eq!(
            store
                .get::<KvCacheDb>(KV_CACHE_NAMESPACE)?
                .expect("metadata")
                .entries[0],
            stored_metadata,
            "unsupported metadata is left unchanged"
        );
        assert_eq!(
            store.get_bytes(&blob_namespace("spill"))?,
            Some(value.sequence.bytes.to_vec()),
            "unsupported payload is neither read nor rewritten"
        );
        drop(store);
        let store = RuntimeStore::open_with_key(&data_dir, [31_u8; 32])?;
        assert!(
            load_persistent_value_or_invalidate_from_store(&store, &stored_metadata)?.is_none()
        );
        assert_eq!(
            store.get_bytes(&blob_namespace("spill"))?,
            Some(value.sequence.bytes.to_vec())
        );
        drop(store);
        std::fs::remove_dir_all(data_dir)?;
        Ok(())
    }

    #[test]
    fn persistent_cache_transaction_enforces_entry_and_byte_budgets() -> Result<()> {
        let data_dir = std::env::temp_dir().join(format!(
            "mom-llama-cache-persistent-budget-{}",
            crate::now_ms()
        ));
        let store = RuntimeStore::open_with_key(&data_dir, [23_u8; 32])?;
        let value = |id: &str, owner: &str, used: u128| {
            let mut value = test_cache_value(id, "fingerprint");
            value.metadata.tier = CacheTier::SessionPersistent;
            value.metadata.owner_id = Some(owner.to_string());
            value.metadata.state_bytes = 4;
            value.metadata.created_at_ms = used;
            value.metadata.last_used_at_ms = used;
            value.sequence.bytes = vec![7; 4].into();
            value
        };
        let old = value("old", "conversation-old", 1);
        let recent = value("recent", "conversation-recent", 2);
        let new = value("new", "conversation-new", 3);
        persist_value_to_store(&store, &old, 2, 8)?;
        persist_value_to_store(&store, &recent, 2, 8)?;
        persist_value_to_store(&store, &new, 2, 8)?;

        let db = store
            .get::<KvCacheDb>(KV_CACHE_NAMESPACE)?
            .ok_or_else(|| anyhow!("cache metadata missing"))?;
        assert_eq!(
            db.entries
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            vec!["new", "recent"]
        );
        assert_eq!(store.get_bytes(&blob_namespace("old"))?, None);
        assert_eq!(
            store.get_bytes(&blob_namespace("recent"))?,
            Some(vec![7; 4])
        );
        assert_eq!(store.get_bytes(&blob_namespace("new"))?, Some(vec![7; 4]));

        let replacement = value("replacement", "conversation-new", 4);
        persist_value_to_store(&store, &replacement, 2, 8)?;
        let db = store
            .get::<KvCacheDb>(KV_CACHE_NAMESPACE)?
            .ok_or_else(|| anyhow!("cache metadata missing"))?;
        assert!(db.entries.iter().any(|entry| entry.id == "replacement"));
        assert!(!db.entries.iter().any(|entry| entry.id == "new"));
        assert_eq!(store.get_bytes(&blob_namespace("new"))?, None);
        assert_eq!(
            store.get_bytes(&blob_namespace("replacement"))?,
            Some(vec![7; 4])
        );
        Ok(())
    }

    #[test]
    fn session_checkpoint_write_is_fenced_by_atomic_conversation_authority() -> Result<()> {
        let data_dir = std::env::temp_dir().join(format!(
            "mom-llama-cache-session-authority-{}",
            crate::now_ms()
        ));
        let store = RuntimeStore::open_with_key(&data_dir, [41_u8; 32])?;
        let conversation_id = Uuid::new_v4().to_string();
        let make_conversation = |title: &str| Conversation {
            id: conversation_id.clone(),
            title: title.to_owned(),
            created_at: "2026-10-17T00:00:00Z".to_owned(),
            updated_at: title.to_owned(),
            kind: crate::conversation_store::ConversationKind::Chat,
            execution_profile: crate::conversation_store::ConversationExecutionProfile::default(),
            selected_model_path: None,
            source_conversation_id: None,
            source_message_id: None,
            branch_root_message_id: None,
            active_leaf_message_id: None,
            recipient_ids: Vec::new(),
            current_skill_ids: Vec::new(),
            messages: Vec::new(),
            inference_window: None,
        };
        let initial = make_conversation("initial");
        let mut db = ConversationDb::default();
        db.conversations.push(initial.clone());
        store.put(crate::conversation_store::CONVERSATIONS_NAMESPACE, &db)?;
        let initial_authority = SessionConversationAuthority::from_conversation(&initial)?;

        let make_value = |id: &str, marker: u8| {
            let mut value = test_cache_value(id, "session-authority-model");
            value.metadata.tier = CacheTier::SessionPersistent;
            value.metadata.owner_id = Some(conversation_id.clone());
            value.metadata.state_bytes = 1;
            value.sequence.bytes = vec![marker].into();
            value
        };

        // A matching source authority may persist normally.
        let first = make_value("session-a", 1);
        assert!(persist_value_to_store_with_authority(
            &store,
            &first,
            8,
            1024,
            Some(&initial_authority),
        )?);

        // Simulate a later source commit, then persist its newer checkpoint.
        // An older writer arriving afterward must neither replace metadata nor
        // delete the newer blob.
        let newer = make_conversation("newer committed source");
        let mut updated_db = db.clone();
        updated_db.conversations[0] = newer.clone();
        store.put(
            crate::conversation_store::CONVERSATIONS_NAMESPACE,
            &updated_db,
        )?;
        let newer_authority = SessionConversationAuthority::from_conversation(&newer)?;
        let second = make_value("session-b", 2);
        assert!(persist_value_to_store_with_authority(
            &store,
            &second,
            8,
            1024,
            Some(&newer_authority),
        )?);
        assert!(!persist_value_to_store_with_authority(
            &store,
            &first,
            8,
            1024,
            Some(&initial_authority),
        )?);
        let persisted = store
            .get::<KvCacheDb>(KV_CACHE_NAMESPACE)?
            .unwrap_or_default();
        assert_eq!(
            persisted.entries.first().map(|entry| entry.id.as_str()),
            Some("session-b")
        );
        assert_eq!(
            store.get_bytes(&blob_namespace("session-b"))?,
            Some(vec![2])
        );
        assert_eq!(store.get_bytes(&blob_namespace("session-a"))?, None);

        // A deleted source conversation also rejects a delayed checkpoint.
        store.put(
            crate::conversation_store::CONVERSATIONS_NAMESPACE,
            &ConversationDb::default(),
        )?;
        assert!(!persist_value_to_store_with_authority(
            &store,
            &second,
            8,
            1024,
            Some(&newer_authority),
        )?);
        assert_eq!(
            store
                .get::<KvCacheDb>(KV_CACHE_NAMESPACE)?
                .unwrap_or_default()
                .entries
                .first()
                .map(|entry| entry.id.as_str()),
            Some("session-b")
        );
        Ok(())
    }

    #[test]
    fn generated_voice_attachment_publication_preserves_session_authority_only_for_exact_owner()
    -> Result<()> {
        let data_dir = std::env::temp_dir().join(format!(
            "mom-llama-cache-generated-voice-authority-{}",
            crate::now_ms()
        ));
        let store = RuntimeStore::open_with_key(&data_dir, [42_u8; 32])?;
        let conversation_id = Uuid::new_v4().to_string();
        let mut original = Conversation {
            id: conversation_id.clone(),
            title: "authority fixture".into(),
            created_at: "1".into(),
            updated_at: "1".into(),
            kind: crate::conversation_store::ConversationKind::Chat,
            execution_profile: Default::default(),
            selected_model_path: None,
            source_conversation_id: None,
            source_message_id: None,
            branch_root_message_id: None,
            active_leaf_message_id: Some("assistant-1".into()),
            recipient_ids: Vec::new(),
            current_skill_ids: Vec::new(),
            messages: vec![crate::conversation_store::Message {
                id: "assistant-1".into(),
                conversation_id: conversation_id.clone(),
                role: crate::conversation_store::MessageRole::Assistant,
                content: "A stable answer.".into(),
                created_at: "1".into(),
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
            inference_window: None,
        };
        let authority = SessionConversationAuthority::from_conversation(&original)?;
        store.put(
            crate::conversation_store::CONVERSATIONS_NAMESPACE,
            &ConversationDb {
                conversations: vec![original.clone()],
                selected_conversation_id: Some(conversation_id.clone()),
            },
        )?;

        let generated_id = "generated-reply-1";
        original.messages[0]
            .attachment_ids
            .push(generated_id.to_string());
        original.updated_at = "2".into();
        store.put(
            crate::conversation_store::CONVERSATIONS_NAMESPACE,
            &ConversationDb {
                conversations: vec![original.clone()],
                selected_conversation_id: Some(conversation_id.clone()),
            },
        )?;
        let mut attachment_db = crate::attachments::AttachmentDb::default();
        attachment_db
            .attachments
            .push(crate::attachments::AttachmentRecord {
                id: generated_id.into(),
                conversation_id: conversation_id.clone(),
                message_id: "assistant-1".into(),
                kind: crate::attachments::AttachmentKind::Audio,
                file_name: "reply.wav".into(),
                source_path: "generated://assistant-voice".into(),
                stored_path: "private://fixture".into(),
                mime: "audio/wav".into(),
                bytes: 48,
                sha256: "0".repeat(64),
                created_at: "2".into(),
                state: crate::attachments::AttachmentState::Committed,
                root_object_id: Some("root".into()),
                detected_format: Some(attachment_native_types::DetectedFormat::Wav),
                coverage: Some(attachment_native_types::Coverage::Complete),
                manifest_namespace: Some("manifest".into()),
                policy_fingerprint: Some("policy".into()),
                artifact_count: 1,
                canonical_text_bytes: 0,
                media_objects: 1,
            });
        store.put(crate::attachments::ATTACHMENTS_NAMESPACE, &attachment_db)?;
        assert!(store.mutate_documents(
            crate::attachments::ATTACHMENTS_NAMESPACE,
            crate::attachments::AttachmentDb::default,
            |_attachments, documents| authority.still_authoritative_in_documents(documents),
        )?);

        let mut wrongly_owned = store
            .get::<crate::attachments::AttachmentDb>(crate::attachments::ATTACHMENTS_NAMESPACE)?
            .expect("generated reply registry");
        wrongly_owned.attachments[0].message_id = "different-assistant".into();
        store.put(crate::attachments::ATTACHMENTS_NAMESPACE, &wrongly_owned)?;
        assert!(
            !store.mutate_documents(
                crate::attachments::ATTACHMENTS_NAMESPACE,
                crate::attachments::AttachmentDb::default,
                |_attachments, documents| authority.still_authoritative_in_documents(documents),
            )?,
            "a moved or spoofed registry owner cannot relax checkpoint authority"
        );
        wrongly_owned.attachments[0].message_id = "assistant-1".into();
        store.put(crate::attachments::ATTACHMENTS_NAMESPACE, &wrongly_owned)?;

        let mut edited = original.clone();
        edited.messages[0].content.push_str(" changed");
        store.put(
            crate::conversation_store::CONVERSATIONS_NAMESPACE,
            &ConversationDb {
                conversations: vec![edited],
                selected_conversation_id: Some(conversation_id.clone()),
            },
        )?;
        assert!(!store.mutate_documents(
            crate::attachments::ATTACHMENTS_NAMESPACE,
            crate::attachments::AttachmentDb::default,
            |_attachments, documents| authority.still_authoritative_in_documents(documents),
        )?);

        let mut ordinary = original;
        ordinary.messages[0]
            .attachment_ids
            .push("ordinary-image".into());
        store.put(
            crate::conversation_store::CONVERSATIONS_NAMESPACE,
            &ConversationDb {
                conversations: vec![ordinary],
                selected_conversation_id: Some(conversation_id),
            },
        )?;
        assert!(!store.mutate_documents(
            crate::attachments::ATTACHMENTS_NAMESPACE,
            crate::attachments::AttachmentDb::default,
            |_attachments, documents| authority.still_authoritative_in_documents(documents),
        )?);
        Ok(())
    }

    #[test]
    fn persistent_cache_rejects_invalid_or_oversized_state() -> Result<()> {
        let data_dir =
            std::env::temp_dir().join(format!("mom-llama-cache-rejection-{}", crate::now_ms()));
        let store = RuntimeStore::open_with_key(&data_dir, [29_u8; 32])?;
        let mut invalid = test_cache_value("invalid", "fingerprint");
        invalid.metadata.token_ids[0] = 99;
        assert!(persist_value_to_store(&store, &invalid, 2, 8).is_err());

        let mut oversized = test_cache_value("oversized", "fingerprint");
        oversized.metadata.state_bytes = 4;
        oversized.sequence.bytes = vec![1; 4].into();
        assert!(persist_value_to_store(&store, &oversized, 2, 3).is_err());
        assert!(
            store
                .get::<KvCacheDb>(KV_CACHE_NAMESPACE)?
                .unwrap_or_default()
                .entries
                .is_empty()
        );
        Ok(())
    }
}
