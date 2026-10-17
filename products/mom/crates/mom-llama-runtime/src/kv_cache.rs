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
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};
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
    pub prefill_us: u64,
    pub verification_us: u64,
    pub persistence_us: u64,
    pub promotion_us: u64,
    pub persisted: bool,
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
static COMMITTED_SESSION_REQUESTS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
static SESSION_CHECKPOINT_SCHEDULER: OnceLock<Arc<SessionCheckpointScheduler>> = OnceLock::new();

struct ScheduledSessionCheckpoint {
    scope: crate::OperationScope,
    request_id: String,
    deadline: Instant,
}

struct SessionCheckpointScheduler {
    pending: Mutex<HashMap<String, ScheduledSessionCheckpoint>>,
    wake: Condvar,
}

pub(crate) fn note_committed_session_request(conversation_id: &str, request_id: &str) {
    let requests = COMMITTED_SESSION_REQUESTS.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut requests) = requests.lock() {
        requests.insert(conversation_id.to_owned(), request_id.to_owned());
    }
}

/// Debounce text-session persistence until there has been a short idle window.
/// The registry retains the committed request even when this bounded queue is
/// full or foreground work preempts the snapshot, so hangup/shutdown can retry.
pub(crate) fn schedule_committed_session_checkpoint(
    scope: &crate::OperationScope,
    conversation_id: &str,
    request_id: &str,
) {
    const MAX_PENDING_CHECKPOINTS: usize = 64;
    let scheduler = SESSION_CHECKPOINT_SCHEDULER.get_or_init(|| {
        let scheduler = Arc::new(SessionCheckpointScheduler {
            pending: Mutex::new(HashMap::new()),
            wake: Condvar::new(),
        });
        let worker = Arc::clone(&scheduler);
        let _ = std::thread::Builder::new()
            .name("mom-session-checkpoint-idle".to_owned())
            .spawn(move || session_checkpoint_worker(worker));
        scheduler
    });
    if let Ok(mut pending) = scheduler.pending.lock() {
        if !pending.contains_key(conversation_id) && pending.len() >= MAX_PENDING_CHECKPOINTS {
            return;
        }
        pending.insert(
            conversation_id.to_owned(),
            ScheduledSessionCheckpoint {
                scope: scope.clone(),
                request_id: request_id.to_owned(),
                deadline: Instant::now() + Duration::from_secs(3),
            },
        );
        scheduler.wake.notify_one();
    }
}

fn session_checkpoint_worker(scheduler: Arc<SessionCheckpointScheduler>) {
    loop {
        let due = {
            let Ok(mut pending) = scheduler.pending.lock() else {
                return;
            };
            loop {
                if pending.is_empty() {
                    let Ok(next) = scheduler.wake.wait(pending) else {
                        return;
                    };
                    pending = next;
                    continue;
                }
                let next_deadline = pending
                    .values()
                    .map(|entry| entry.deadline)
                    .min()
                    .unwrap_or_else(Instant::now);
                let now = Instant::now();
                if next_deadline > now {
                    let Ok((next, _)) = scheduler.wake.wait_timeout(pending, next_deadline - now)
                    else {
                        return;
                    };
                    pending = next;
                    continue;
                }
                let key = pending
                    .iter()
                    .filter(|(_, value)| value.deadline <= now)
                    .min_by_key(|(_, value)| value.deadline)
                    .map(|(key, _)| key.clone());
                break key.and_then(|key| pending.remove(&key).map(|entry| (key, entry)));
            }
        };
        let Some((conversation_id, scheduled)) = due else {
            continue;
        };
        let _ = checkpoint_committed_session_request(
            &scheduled.scope,
            &conversation_id,
            Some(&scheduled.request_id),
        );
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
    let operation = "mom_llama.kv_cache_prepare_voice_persona_resident";
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
    let fingerprint = cache_fingerprint(&handle)?;
    if let Some(metadata) = restore_latest_session_checkpoint_if_needed(
        &settings,
        &handle,
        &fingerprint,
        &session_owner_id,
    )? {
        return Ok(CommandResult::passed(
            operation,
            "prompt_smoke_verified",
            VoicePersonaPrefixPreparation {
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
            "prompt_smoke_verified",
            VoicePersonaPrefixPreparation {
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
            "prompt_smoke_verified",
            VoicePersonaPrefixPreparation {
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
                    "prompt_smoke_verified",
                    VoicePersonaPrefixPreparation {
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
            live_value = load_persistent_value_or_invalidate(&settings, metadata, &handle)?;
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
        ) {
            handle
                .restore_sequence(value.sequence.clone(), 0)
                .map_err(|error| anyhow!(error))?;
        }
        return Ok(CommandResult::passed(
            operation,
            "prompt_smoke_verified",
            VoicePersonaPrefixPreparation {
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
    let value = create_prefix_value(
        &handle,
        CacheTier::PersonaPack,
        Some(persona.id.clone()),
        format!("Resident voice persona: {}", persona.title),
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
    let store = RuntimeStore::open(&settings.data_dir)?;
    let Some(lease) = MomCachePromotionLease::acquire(&store, Some(&persona.id))? else {
        return Ok(CommandResult::blocked(
            operation,
            "stub_blocked",
            Blocker::new(
                "kv_cache_owner_removed",
                "The persona was removed during preparation.",
                Vec::new(),
            ),
        ));
    };
    if !lease.promote(value.clone())? && !lease.current_for(value.metadata.owner_id.as_deref())? {
        return Ok(CommandResult::blocked(
            operation,
            "stub_blocked",
            Blocker::new(
                "kv_cache_owner_changed",
                "The persona changed during preparation.",
                Vec::new(),
            ),
        ));
    }
    let promotion_us = started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
    drop(lease);
    let persistence_started = std::time::Instant::now();
    let persisted = persist_value(&settings, &value)?;
    let persistence_us = persistence_started
        .elapsed()
        .as_micros()
        .min(u128::from(u64::MAX)) as u64;
    Ok(CommandResult::passed(
        operation,
        "prompt_smoke_verified",
        VoicePersonaPrefixPreparation {
            resident_prefix_tokens: value.sequence.token_count,
            metadata: Some(value.metadata.clone()),
            prefill_us,
            verification_us,
            persistence_us,
            promotion_us,
            persisted,
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
    let value = load_persistent_value_or_invalidate(&settings, &metadata, &handle)?;
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
    let Some(current_value) =
        load_current_persistent_value(&store, &verified_value.metadata.id, &handle)?
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

pub fn kv_cache_clear(scope: &crate::OperationScope) -> Result<CommandResult<KvCacheStatus>> {
    let settings = resolve_settings()?;
    clear_native_prefix_cache(scope, &settings)?;
    let store = RuntimeStore::open(&settings.data_dir)?;
    store.mutate_documents(
        KV_CACHE_NAMESPACE,
        KvCacheDb::default,
        |db: &mut KvCacheDb, documents| {
            for entry in &db.entries {
                delete_cache_bytes(documents, entry);
            }
            db.entries.clear();
            Ok(())
        },
    )?;
    if let Ok(mut cache) = memory_cache().lock() {
        cache.clear();
    }
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
    let Some(current) = load_current_persistent_value(&store, &metadata.id, handle)? else {
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
    let kind = handle
        .restore_sequence(value.sequence.clone(), 0)
        .map_err(|error| anyhow!(error))?;
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
    let fingerprint = cache_fingerprint(&handle)?;
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
    let Some(value) =
        load_persistent_value_or_invalidate_from_store(&store, &metadata, |receipt| {
            handle.has_live_sequence_receipt(receipt)
        })?
    else {
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
    let fingerprint = cache_fingerprint(&handle)?;
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
    checkpoint_committed_session_request(scope, conversation_id, None)
}

/// Best-effort checkpoint of other committed conversations before an
/// ordinary chat request can replace their resident request authority.
pub(crate) fn checkpoint_pending_sessions_before_chat(
    scope: &crate::OperationScope,
    incoming_conversation_id: &str,
) {
    if scope.has_active_voice_call() {
        return;
    }
    let requests = COMMITTED_SESSION_REQUESTS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .map(|requests| {
            pending_session_checkpoint_candidates(&requests, Some(incoming_conversation_id))
        })
        .unwrap_or_default();
    for (conversation_id, request_id) in requests {
        if let Err(error) =
            checkpoint_committed_session_request(scope, &conversation_id, Some(&request_id))
        {
            eprintln!(
                "Unable to checkpoint committed conversation {conversation_id} before chat: {error:#}"
            );
        }
    }
}

/// Capture a conversation using its current frozen profile before that profile
/// is changed. Cache failures are diagnostic only; model selection remains
/// available and the pending checkpoint is still eligible for later retry.
pub(crate) fn checkpoint_session_before_profile_change(
    scope: &crate::OperationScope,
    conversation_id: &str,
) {
    if scope.has_active_voice_call() {
        return;
    }
    if let Err(error) = checkpoint_committed_session_request(scope, conversation_id, None) {
        eprintln!(
            "Unable to checkpoint committed conversation {conversation_id} before profile change: {error:#}"
        );
    }
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
    let requests = COMMITTED_SESSION_REQUESTS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .map(|requests| {
            requests
                .iter()
                .map(|(conversation_id, request_id)| (conversation_id.clone(), request_id.clone()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for (conversation_id, request_id) in requests {
        schedule_committed_session_checkpoint(scope, &conversation_id, &request_id);
    }
}

fn pending_session_checkpoint_candidates(
    requests: &HashMap<String, String>,
    excluded_conversation_id: Option<&str>,
) -> Vec<(String, String)> {
    requests
        .iter()
        .filter(|(conversation_id, _)| Some(conversation_id.as_str()) != excluded_conversation_id)
        .map(|(conversation_id, request_id)| (conversation_id.clone(), request_id.clone()))
        .collect()
}

/// Persist every latest committed request the resident worker still owns.
/// Call from graceful shutdown after operation cancellation/drain and before
/// native model teardown. Ineligible or over-budget entries remain registered
/// only for this process and are safely skipped.
pub fn flush_committed_session_checkpoints(scope: &crate::OperationScope) -> Result<usize> {
    // Graceful shutdown reaches this only after application work and model
    // supervisors drain. Clearing the lifecycle markers here lets the final
    // exact-committed snapshots proceed before native teardown.
    scope
        .clear_voice_call_activity()
        .map_err(|error| anyhow!("Unable to clear voice call activity at shutdown: {error:#}"))?;
    let requests = COMMITTED_SESSION_REQUESTS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .map_err(|_| anyhow!("Committed session checkpoint registry is unavailable"))?
        .iter()
        .map(|(conversation_id, request_id)| (conversation_id.clone(), request_id.clone()))
        .collect::<Vec<_>>();
    let mut persisted = 0;
    let mut first_error = None;
    for (conversation_id, request_id) in requests {
        match checkpoint_committed_session_request(scope, &conversation_id, Some(&request_id)) {
            Ok(Some(_)) => persisted += 1,
            Ok(None) => {}
            Err(error) => {
                first_error.get_or_insert(error);
            }
        }
    }
    if let Some(error) = first_error {
        return Err(anyhow!(
            "one or more committed session checkpoints failed after {persisted} saves: {error:#}"
        ));
    }
    Ok(persisted)
}

fn checkpoint_committed_session_request(
    scope: &crate::OperationScope,
    conversation_id: &str,
    expected_request_id: Option<&str>,
) -> Result<Option<String>> {
    if scope.has_active_voice_call() {
        return Ok(None);
    }
    let mut settings = resolve_settings()?;
    if !settings.kv_cache_policy.persists_conversation_checkpoints() {
        return Ok(None);
    }
    if scope
        .current_chat_request_id(conversation_id, true)
        .is_some()
    {
        return Ok(None);
    }
    let request_id = COMMITTED_SESSION_REQUESTS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .map_err(|_| anyhow!("Committed voice-session checkpoint registry is unavailable"))?
        .get(conversation_id)
        .cloned();
    let Some(request_id) = request_id else {
        return Ok(None);
    };
    if expected_request_id.is_some_and(|expected| expected != request_id) {
        return Ok(None);
    }
    let conversations = crate::conversation_store::load_db_with_settings(&settings)?;
    let Some(conversation) = conversations
        .conversations
        .iter()
        .find(|conversation| conversation.id == conversation_id)
    else {
        return Ok(None);
    };
    crate::chat::apply_conversation_model_pair(
        &mut settings,
        conversation.execution_profile.model_path.clone(),
        conversation.selected_model_path.clone(),
        conversation.execution_profile.mmproj_path.clone(),
    );
    let Some(model_path) = settings.model_path.clone() else {
        return Ok(None);
    };
    let Some(handle) = resident_model_for_profile_if_loaded(
        scope,
        &settings,
        &model_path,
        settings.mmproj_path.as_deref(),
    )
    .map_err(|blocked| anyhow!(blocked.blocker.message))?
    else {
        return Ok(None);
    };
    let Some(state) = scope
        .with_idle_voice_checkpoint(|| {
            handle
                .snapshot_conversation_for_request_if_fits(&request_id, MAX_PERSISTENT_ENTRY_BYTES)
                .map_err(|error| anyhow!(error))
        })?
        .flatten()
    else {
        return Ok(None);
    };
    if (state.token_ids.is_empty() && state.cells.is_empty()) || state.bytes.is_empty() {
        return Ok(None);
    }
    let fingerprint = cache_fingerprint(&handle)?;
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
        return Ok(None);
    }
    let promoted = persist_value(&settings, &value)?;
    if !promoted {
        return Ok(None);
    }
    if let Ok(mut requests) = COMMITTED_SESSION_REQUESTS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        && requests.get(conversation_id) == Some(&request_id)
    {
        requests.remove(conversation_id);
    }
    Ok(Some(value.metadata.id))
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

fn create_prefix_value(
    handle: &NativeModelHandle,
    tier: CacheTier,
    owner_id: Option<String>,
    label: String,
    stable_messages: &[ChatMessage],
    template: ChatTemplateChoice,
) -> Result<PrefixCacheValue> {
    let settings = resolve_settings()?;
    let tokenized = handle
        .tokenize_messages_with_template(stable_messages.to_vec(), template.clone())
        .map_err(|error| anyhow!(error))?;
    let cached_prefix = compatible_cached_prefix_for_owner_with_settings(
        handle,
        stable_messages,
        owner_id.as_deref(),
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
    let state = handle
        .prefill_shared_prefix(SharedPrefixBatchRequest {
            request_id: format!("kv-cache-prefill-{}", Uuid::new_v4()),
            model_id: handle.status().model_id,
            common_messages: stable_messages.to_vec(),
            chat_template: template,
            branches,
            cached_prefix,
        })
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

fn persistent_entry_fits(bytes: usize) -> bool {
    bytes > 0 && bytes <= MAX_PERSISTENT_ENTRY_BYTES
}

fn persist_value_with_promotion_hook(
    store: &RuntimeStore,
    value: &PrefixCacheValue,
    before_promotion: impl FnOnce(),
) -> Result<bool> {
    let Some(lease) = MomCachePromotionLease::acquire(store, value.metadata.owner_id.as_deref())?
    else {
        return Ok(false);
    };
    if !persist_value_to_store(
        store,
        value,
        DEFAULT_PERSISTENT_CACHE_ENTRIES,
        DEFAULT_PERSISTENT_CACHE_BYTES,
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

fn persist_value_to_store(
    store: &RuntimeStore,
    value: &PrefixCacheValue,
    max_entries: usize,
    max_bytes: usize,
) -> Result<bool> {
    let mut persisted = value.clone();
    persisted.bind_reconstruction();
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
    store.mutate_documents(
        KV_CACHE_NAMESPACE,
        KvCacheDb::default,
        |db: &mut KvCacheDb, documents| {
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
            write_cache_bytes(documents, &value.metadata.id, &value.sequence.bytes)?;
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
    )
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
    handle: &NativeModelHandle,
) -> Result<Option<PrefixCacheValue>> {
    let store = RuntimeStore::open(&settings.data_dir)?;
    load_persistent_value_or_invalidate_from_store(&store, metadata, |receipt| {
        handle.has_live_sequence_receipt(receipt)
    })
}

fn load_persistent_value_or_invalidate_from_store(
    store: &RuntimeStore,
    metadata: &PrefixCacheMetadata,
    has_live_receipt: impl FnOnce(&[u8; 32]) -> bool,
) -> Result<Option<PrefixCacheValue>> {
    let Some(reconstruction) = metadata.reconstruction.as_ref() else {
        invalidate_persistent_entry_in_store(store, &metadata.id)?;
        return Ok(None);
    };
    if reconstruction.token_replay_allowed != metadata.cells.is_empty() {
        invalidate_persistent_entry_in_store(store, &metadata.id)?;
        return Ok(None);
    }
    // Authenticated exports survive worker/process restart. Their full
    // bytes must reach the engine's signature and fingerprint check; replacing
    // them with token replay here destroyed the durable KV cache on launch.
    let authenticated_export = reconstruction.header.starts_with(b"native-seq-v2\0\0\0");
    if !authenticated_export && !has_live_receipt(&reconstruction.export_receipt) {
        let Some(value) = metadata.replay_value() else {
            invalidate_persistent_entry_in_store(store, &metadata.id)?;
            return Ok(None);
        };
        if !persistent_entry_fits(value.metadata.state_bytes) {
            invalidate_persistent_entry_in_store(store, &metadata.id)?;
            return Ok(None);
        }
        return store.mutate_documents(KV_CACHE_NAMESPACE, KvCacheDb::default, |db, documents| {
            if let Some(owner_id) = metadata.owner_id.as_deref()
                && crate::personas::persona_cache_owner_is_removed_from_documents(
                    documents, owner_id,
                )?
            {
                return Ok(None);
            }
            let Some(current) = db.entries.iter_mut().find(|entry| entry.id == metadata.id) else {
                return Ok(None);
            };
            if current != metadata {
                return Ok(None);
            }
            // Expired native bytes are overwritten directly, never materialized.
            if value.metadata.state_bytes != metadata.state_bytes {
                delete_cache_bytes(documents, metadata);
                write_cache_bytes(documents, &metadata.id, &value.sequence.bytes)?;
                *current = value.metadata.clone();
            }
            Ok(Some(value.clone()))
        });
    }
    let loaded = read_cache_bytes(store, metadata);
    let value = match loaded {
        Ok(Some(bytes)) => PrefixCacheValue {
            metadata: metadata.clone(),
            sequence: SequenceStateBlob {
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
        },
        Ok(None) | Err(_) => {
            invalidate_persistent_entry_in_store(store, &metadata.id)?;
            return Ok(None);
        }
    };
    if !value.is_valid() {
        invalidate_persistent_entry_in_store(store, &metadata.id)?;
        return Ok(None);
    }
    Ok(Some(value))
}

fn load_current_persistent_value(
    store: &RuntimeStore,
    cache_id: &str,
    handle: &NativeModelHandle,
) -> Result<Option<PrefixCacheValue>> {
    let db = store
        .get::<KvCacheDb>(KV_CACHE_NAMESPACE)?
        .unwrap_or_default();
    let Some(metadata) = db.entries.iter().find(|entry| entry.id == cache_id) else {
        return Ok(None);
    };
    load_persistent_value_or_invalidate_from_store(store, metadata, |receipt| {
        handle.has_live_sequence_receipt(receipt)
    })
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
        && left.token_sha256 == right.token_sha256
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

fn write_cache_bytes(
    documents: &mut DocumentMutations<'_, '_, '_>,
    id: &str,
    bytes: &[u8],
) -> Result<()> {
    if !persistent_entry_fits(bytes.len()) {
        return Err(anyhow!(
            "native prefix state exceeds the per-entry persistent cache limit"
        ));
    }
    for (index, chunk) in bytes.chunks(CACHE_BLOB_CHUNK_BYTES).enumerate() {
        documents.put_bytes_streamed(&blob_chunk_namespace(id, index), chunk)?;
    }
    Ok(())
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

fn read_cache_bytes(
    store: &RuntimeStore,
    metadata: &PrefixCacheMetadata,
) -> Result<Option<Vec<u8>>> {
    if !persistent_entry_fits(metadata.state_bytes) {
        return Ok(None);
    }
    store.read_documents(|snapshot| {
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(metadata.state_bytes)?;
        for index in 0..metadata.state_bytes.div_ceil(CACHE_BLOB_CHUNK_BYTES) {
            let Some(chunk) = snapshot.get_bytes(&blob_chunk_namespace(&metadata.id, index))?
            else {
                return Ok(None);
            };
            let expected = (metadata.state_bytes - bytes.len()).min(CACHE_BLOB_CHUNK_BYTES);
            if chunk.len() != expected {
                return Ok(None);
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(Some(bytes))
    })
}

fn encrypted_blob_uri(id: &str) -> String {
    format!("encrypted://{}", blob_namespace(id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation_store::{Conversation, ConversationDb};
    use llama_native_cache::ValidatedPrefixCacheValue;
    use sha2::{Digest, Sha256};
    use std::sync::{Arc, Barrier};
    use std::thread;

    #[derive(Debug, Deserialize)]
    struct W1CacheCorruptionFixture {
        schema: String,
        fixture_key_hex: String,
        native_prefix_namespace: String,
        session_metadata_namespace: String,
        session_blob_namespace: String,
        authoritative_namespace: String,
        authoritative_conversation: Conversation,
        tampered_ciphertext_hex: String,
        native_prefix_disposition: String,
        session_blob_disposition: String,
    }

    #[derive(Serialize)]
    struct W1SessionCacheLogicalState<'a> {
        metadata: &'a KvCacheDb,
        blob_sha256: String,
        blob_length: usize,
    }

    #[derive(Serialize)]
    struct W1SessionCacheAfterLogicalState<'a> {
        metadata: &'a KvCacheDb,
        blob: Option<&'a [u8]>,
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
                sequence_id: 0,
                token_count: 1,
                bytes: (vec![1]).into(),
                token_ids: vec![1],
                cells: Vec::new(),
            },
        }
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
    fn pre_chat_checkpoint_candidates_exclude_only_the_incoming_conversation() {
        let requests = HashMap::from([
            ("incoming".to_owned(), "request-a".to_owned()),
            ("older-one".to_owned(), "request-b".to_owned()),
            ("older-two".to_owned(), "request-c".to_owned()),
        ]);
        let candidates = pending_session_checkpoint_candidates(&requests, Some("incoming"));
        assert_eq!(candidates.len(), 2);
        assert!(candidates.contains(&("older-one".to_owned(), "request-b".to_owned())));
        assert!(candidates.contains(&("older-two".to_owned(), "request-c".to_owned())));
        assert!(
            !candidates
                .iter()
                .any(|(conversation_id, _)| conversation_id == "incoming")
        );
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
        assert!(DEFAULT_PERSISTENT_CACHE_BYTES >= 8 * MAX_PERSISTENT_ENTRY_BYTES);
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
    fn persistent_cache_corruption_invalidates_and_falls_back_after_reopen() -> Result<()> {
        let fixture: W1CacheCorruptionFixture =
            serde_json::from_str(include_str!("../fixtures/compat/cache-corruption-v1.json"))?;
        assert_eq!(
            fixture.schema,
            "mom_llama.w1.disposable_cache_corruption_fixture.v1"
        );
        assert_eq!(fixture.fixture_key_hex, "17".repeat(32));
        assert_eq!(fixture.session_metadata_namespace, KV_CACHE_NAMESPACE);
        assert_eq!(
            fixture.native_prefix_disposition,
            "quarantine_then_cold_miss"
        );
        assert_eq!(
            fixture.session_blob_disposition,
            "invalidate_delete_then_cold_miss"
        );
        assert_eq!(fixture.tampered_ciphertext_hex, "00");
        let data_dir =
            std::env::temp_dir().join(format!("mom-llama-cache-corruption-{}", crate::now_ms()));
        let store = RuntimeStore::open_with_key(&data_dir, [17_u8; 32])?;
        let mut value = test_cache_value("w1-tampered", "fingerprint");
        value.metadata.tier = CacheTier::SessionPersistent;
        store.mutate_documents(
            KV_CACHE_NAMESPACE,
            KvCacheDb::default,
            |db: &mut KvCacheDb, documents| {
                db.entries.push(value.metadata.clone());
                documents.put_bytes(&blob_namespace(&value.metadata.id), &value.sequence.bytes)?;
                Ok(())
            },
        )?;
        assert_eq!(
            blob_namespace(&value.metadata.id),
            fixture.session_blob_namespace
        );
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
        let metadata_before = store
            .get::<KvCacheDb>(KV_CACHE_NAMESPACE)?
            .ok_or_else(|| anyhow!("cache metadata missing before corruption"))?;
        let mut session_before = serde_json::to_vec_pretty(&W1SessionCacheLogicalState {
            metadata: &metadata_before,
            blob_sha256: format!("{:x}", Sha256::digest(value.sequence.bytes.as_slice())),
            blob_length: value.sequence.bytes.len(),
        })?;
        session_before.push(b'\n');
        assert_eq!(
            session_before,
            include_bytes!("../fixtures/compat/cache-session-state-v1.json"),
            "{}",
            String::from_utf8_lossy(&session_before)
        );
        let connection = rusqlite::Connection::open(store.path())?;
        connection.execute(
            "UPDATE encrypted_documents SET ciphertext = X'00' WHERE namespace = ?1",
            [blob_namespace(&value.metadata.id)],
        )?;
        connection.execute(
            "UPDATE encrypted_documents SET ciphertext = X'00' WHERE namespace = ?1",
            [&fixture.native_prefix_namespace],
        )?;

        assert!(
            load_persistent_value_or_invalidate_from_store(&store, &value.metadata, |_| true)?
                .is_none()
        );
        assert_eq!(
            store
                .get_disposable_cache::<Vec<PrefixCacheValue>>(&fixture.native_prefix_namespace)?,
            None
        );
        let db = store
            .get::<KvCacheDb>(KV_CACHE_NAMESPACE)?
            .ok_or_else(|| anyhow!("cache metadata missing"))?;
        assert_eq!(db.entries[0].state, CacheEntryState::Invalidated);
        assert_eq!(store.get_bytes(&blob_namespace(&value.metadata.id))?, None);
        assert_eq!(
            store.get::<ConversationDb>(&fixture.authoritative_namespace)?,
            Some(authoritative.clone())
        );
        drop(connection);
        drop(store);
        let store = RuntimeStore::open_with_key(&data_dir, [17_u8; 32])?;
        assert!(
            load_persistent_value_or_invalidate_from_store(&store, &value.metadata, |_| true)?
                .is_none()
        );
        let native_after = store
            .get_disposable_cache::<Vec<PrefixCacheValue>>(&fixture.native_prefix_namespace)?;
        assert_eq!(native_after, None);
        let session_after_metadata = store
            .get::<KvCacheDb>(KV_CACHE_NAMESPACE)?
            .ok_or_else(|| anyhow!("cache metadata missing after reopen"))?;
        let session_after_blob = store.get_bytes(&blob_namespace(&value.metadata.id))?;
        let mut native_after_bytes = serde_json::to_vec_pretty(&native_after)?;
        native_after_bytes.push(b'\n');
        assert_eq!(
            native_after_bytes,
            include_bytes!("../fixtures/compat/cache-native-prefix-after-state-v1.json")
        );
        let mut session_after_bytes =
            serde_json::to_vec_pretty(&W1SessionCacheAfterLogicalState {
                metadata: &session_after_metadata,
                blob: session_after_blob.as_deref(),
            })?;
        session_after_bytes.push(b'\n');
        assert_eq!(
            session_after_bytes,
            include_bytes!("../fixtures/compat/cache-session-after-state-v1.json")
        );
        assert_eq!(
            store.get::<ConversationDb>(&fixture.authoritative_namespace)?,
            Some(authoritative.clone())
        );

        let mut missing = test_cache_value("missing", "fingerprint");
        missing.metadata.tier = CacheTier::SessionPersistent;
        store.mutate(KV_CACHE_NAMESPACE, KvCacheDb::default, |db| {
            db.entries.push(missing.metadata.clone());
            Ok(())
        })?;
        assert!(
            load_persistent_value_or_invalidate_from_store(&store, &missing.metadata, |_| true)?
                .is_none()
        );
        let db = store
            .get::<KvCacheDb>(KV_CACHE_NAMESPACE)?
            .ok_or_else(|| anyhow!("cache metadata missing"))?;
        assert_eq!(
            db.entries
                .iter()
                .find(|entry| entry.id == "missing")
                .map(|entry| entry.state),
            Some(CacheEntryState::Invalidated)
        );
        let quarantine_rows: i64 = rusqlite::Connection::open(store.path())?.query_row(
            "SELECT COUNT(*) FROM encrypted_documents WHERE namespace LIKE 'quarantine.disposable-cache.%'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(quarantine_rows, 1);

        assert_eq!(
            store.get::<ConversationDb>(&fixture.authoritative_namespace)?,
            Some(authoritative)
        );
        Ok(())
    }

    #[test]
    fn expired_spill_reconstructs_without_decrypting_native_payload_and_preserves_live_spill()
    -> Result<()> {
        let data_dir = std::env::temp_dir().join(format!("mom-cache-lifetime-{}", Uuid::new_v4()));
        let store = RuntimeStore::open_with_key(&data_dir, [31_u8; 32])?;
        let mut value = test_cache_value("spill", "fingerprint");
        value.metadata.tier = CacheTier::SessionPersistent;
        value.sequence.bytes = vec![7; 64 * 1024].into();
        value.sequence.sequence_id = 3;
        value.metadata.state_bytes = value.sequence.bytes.len();
        value.bind_reconstruction();
        persist_value_to_store(&store, &value, 4, 1024 * 1024)?;
        let live =
            load_persistent_value_or_invalidate_from_store(&store, &value.metadata, |receipt| {
                assert_eq!(receipt, &value.sequence.export_receipt());
                true
            })?
            .expect("same-worker spill");
        assert_eq!(live.sequence, value.sequence);
        let connection = rusqlite::Connection::open(store.path())?;
        connection.execute(
            "UPDATE encrypted_documents SET ciphertext = X'00' WHERE namespace = ?1",
            [blob_namespace("spill")],
        )?;
        assert!(store.get_bytes(&blob_namespace("spill")).is_err());
        let replay =
            load_persistent_value_or_invalidate_from_store(&store, &value.metadata, |_| false)?
                .expect("expired receipt reconstructs");
        assert_eq!(
            replay.sequence.bytes.len(),
            llama_native_types::SEQUENCE_STATE_HEADER_BYTES
        );
        assert_eq!(replay.sequence.token_ids, value.sequence.token_ids);
        assert_eq!(replay.metadata.fingerprint, value.metadata.fingerprint);
        assert_eq!(
            store
                .get_bytes(&blob_namespace("spill"))?
                .expect("compacted")
                .len(),
            llama_native_types::SEQUENCE_STATE_HEADER_BYTES
        );
        assert!(
            load_persistent_value_or_invalidate_from_store(&store, &value.metadata, |_| false)?
                .is_none(),
            "stale metadata cannot overwrite compacted or replaced entry"
        );
        drop(connection);
        drop(store);
        let store = RuntimeStore::open_with_key(&data_dir, [31_u8; 32])?;
        let restored =
            load_persistent_value_or_invalidate_from_store(&store, &replay.metadata, |_| false)?
                .expect("restart replay");
        assert_eq!(restored.sequence, replay.sequence);
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
    fn persistent_cache_rejects_invalid_or_oversized_state() -> Result<()> {
        let data_dir =
            std::env::temp_dir().join(format!("mom-llama-cache-rejection-{}", crate::now_ms()));
        let store = RuntimeStore::open_with_key(&data_dir, [29_u8; 32])?;
        let mut invalid = test_cache_value("invalid", "fingerprint");
        invalid.metadata.token_sha256 = "wrong".to_string();
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
