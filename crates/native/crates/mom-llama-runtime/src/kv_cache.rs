use crate::config::{KvCachePolicy, Settings, resolve_settings, upstream_setting_string};
use crate::native_runtime::resident_model;
use crate::now_ms;
use crate::receipts::{Blocker, CommandResult};
use crate::skill_store::load_skill_db;
use crate::store::RuntimeStore;
use anyhow::{Result, anyhow};
use llama_native_cache::{
    CacheEntryState, CacheFingerprint, CacheMatch, CacheTier, MemoryPrefixCache,
    PrefixCacheMetadata, PrefixCacheValue, longest_compatible_prefix,
    longest_compatible_prefix_for_owner,
};
use llama_native_engine::NativeModelHandle;
use llama_native_types::{
    BranchRequest, ChatMessage, ChatRole, ChatTemplateChoice, GenerationInput, GenerationRequest,
    GenerationState, NativeErrorCode, PromptForm, PromptTokenPolicy, SamplingConfig,
    SequenceStateBlob, SharedPrefixBatchRequest,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use uuid::Uuid;

const KV_CACHE_NAMESPACE: &str = "kv-cache.v3";
const KV_CACHE_BLOB_PREFIX: &str = "kv-cache.v3.blob.";
const DEFAULT_MEMORY_CACHE_BYTES: usize = 256 * 1024 * 1024;
const DEFAULT_PERSISTENT_CACHE_BYTES: usize = 2 * 1024 * 1024 * 1024;
const DEFAULT_PERSISTENT_CACHE_ENTRIES: usize = 64;
const BUILTIN_CACHE_OWNER: &str = "builtin:kind-local-assistant";
const BUILTIN_CACHE_PROMPT: &str =
    "You are a kind, concise local assistant. Answer clearly and acknowledge uncertainty.";

pub type KvCacheMetadata = PrefixCacheMetadata;

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

static MEMORY_CACHE: OnceLock<Mutex<MemoryPrefixCache>> = OnceLock::new();

fn memory_cache() -> &'static Mutex<MemoryPrefixCache> {
    MEMORY_CACHE.get_or_init(|| Mutex::new(MemoryPrefixCache::new(DEFAULT_MEMORY_CACHE_BYTES)))
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

pub fn kv_cache_save(skill_id: Option<String>) -> Result<CommandResult<KvCacheMetadata>> {
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
    let handle = match resident_model(&settings) {
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
    )?;
    if !verify_restore_equivalence(&handle, &stable_messages, &value.sequence)? {
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
    persist_value(&settings, &value)?;
    promote_to_memory(value.clone());
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

pub fn kv_cache_restore(cache_id: Option<String>) -> Result<CommandResult<KvCacheMetadata>> {
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
    let handle = match resident_model(&settings) {
        Ok(handle) => handle,
        Err(blocked) => {
            return Ok(CommandResult::blocked(
                "mom_llama.kv_cache_restore",
                &blocked.readiness,
                blocked.blocker,
            ));
        }
    };
    let mut db = load_db()?;
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
        verify_restore_equivalence(&handle, &stable_messages, &value.sequence)?
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
    let value = value.ok_or_else(|| anyhow!("compatible cache value disappeared"))?;
    handle
        .restore_sequence(value.sequence.clone(), 0)
        .map_err(|error| anyhow!(error))?;
    db.entries[index].last_used_at_ms = now_ms();
    let restored = db.entries[index].clone();
    save_db(&db)?;
    promote_to_memory(value);
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
        Vec::new(),
        true,
        false,
    ))
}

pub fn kv_cache_clear() -> Result<CommandResult<KvCacheStatus>> {
    let settings = resolve_settings()?;
    let store = RuntimeStore::open(&settings.data_dir)?;
    store.mutate_documents(
        KV_CACHE_NAMESPACE,
        KvCacheDb::default,
        |db: &mut KvCacheDb, documents| {
            for entry in &db.entries {
                documents.delete(&blob_namespace(&entry.id));
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

pub fn compatible_cached_prefix(
    handle: &NativeModelHandle,
    messages: &[ChatMessage],
) -> Result<Option<(String, SequenceStateBlob)>> {
    compatible_cached_prefix_for_owner(handle, messages, None)
}

fn compatible_cached_prefix_for_owner(
    handle: &NativeModelHandle,
    messages: &[ChatMessage],
    owner_id: Option<&str>,
) -> Result<Option<(String, SequenceStateBlob)>> {
    if messages.is_empty() {
        return Ok(None);
    }
    let settings = resolve_settings()?;
    if !settings.kv_cache_policy.allows_prefix_reuse() {
        return Ok(None);
    }
    let fingerprint = cache_fingerprint(handle)?;
    let tokenized = handle
        .tokenize_messages(messages.to_vec())
        .map_err(|error| anyhow!(error))?;
    let now = now_ms();
    let memory_match = memory_cache().lock().ok().and_then(|cache| match owner_id {
        Some(owner) => cache.best_match_for_owner(&fingerprint, &tokenized.token_ids, owner),
        None => cache.best_match(&fingerprint, &tokenized.token_ids),
    });
    let mut db = load_db()?;
    let persistent_match = match owner_id {
        Some(owner) => longest_compatible_prefix_for_owner(
            &db.entries,
            &fingerprint,
            &tokenized.token_ids,
            Some(owner),
        ),
        None => longest_compatible_prefix(&db.entries, &fingerprint, &tokenized.token_ids),
    };
    let selected = choose_match(memory_match, persistent_match);
    let Some(selected) = selected else {
        return Ok(None);
    };
    if selected.tier == CacheTier::MemoryLru {
        let value = memory_cache()
            .lock()
            .ok()
            .and_then(|mut cache| cache.get(&selected.id, now));
        return Ok(value
            .filter(PrefixCacheValue::is_valid)
            .map(|value| (value.metadata.id, value.sequence)));
    }
    let Some(index) = db.entries.iter().position(|entry| entry.id == selected.id) else {
        return Ok(None);
    };
    let metadata = db.entries[index].clone();
    let Some(mut value) = load_persistent_value_or_invalidate(&settings, &metadata)? else {
        return Ok(None);
    };
    if !value.is_valid() {
        invalidate_persistent_entry(&settings, &metadata.id)?;
        return Ok(None);
    }
    db.entries[index].last_used_at_ms = now;
    value.metadata.last_used_at_ms = now;
    save_db(&db)?;
    promote_to_memory(value.clone());
    Ok(Some((value.metadata.id, value.sequence)))
}

#[derive(Debug, Clone)]
pub(crate) struct PersonaPrefixUse {
    pub cache_id: String,
    pub sequence: SequenceStateBlob,
    pub reused: bool,
}

pub(crate) fn ensure_persona_prefix(
    handle: &NativeModelHandle,
    owner_id: &str,
    label: &str,
    stable_messages: &[ChatMessage],
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
            reused: true,
        }));
    }
    let value = create_prefix_value(
        handle,
        CacheTier::PersonaPack,
        Some(owner_id.to_string()),
        label.to_string(),
        stable_messages,
    )?;
    persist_value(&settings, &value)?;
    promote_to_memory(value);
    Ok(
        compatible_cached_prefix_for_owner(handle, target_messages, Some(owner_id))?.map(
            |(cache_id, sequence)| PersonaPrefixUse {
                cache_id,
                sequence,
                reused: false,
            },
        ),
    )
}

pub fn persist_session_checkpoint(
    conversation_id: &str,
    handle: &NativeModelHandle,
) -> Result<Option<String>> {
    let settings = resolve_settings()?;
    if !settings.kv_cache_policy.persists_conversation_checkpoints() {
        return Ok(None);
    }
    let state = handle
        .snapshot_sequence(0)
        .map_err(|error| anyhow!(error))?;
    if state.token_ids.is_empty() || state.bytes.is_empty() {
        return Ok(None);
    }
    let fingerprint = cache_fingerprint(handle)?;
    let metadata = PrefixCacheMetadata::new(
        Uuid::new_v4().to_string(),
        CacheTier::SessionPersistent,
        fingerprint,
        state.token_ids.clone(),
        state.bytes.len(),
        now_ms(),
    )
    .with_owner(conversation_id)
    .with_label(format!("Conversation {conversation_id}"));
    let value = PrefixCacheValue {
        metadata,
        sequence: state,
    };
    if !value.is_valid() {
        return Ok(None);
    }
    persist_value(&settings, &value)?;
    promote_to_memory(value.clone());
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

pub fn latest_verified_cache_path() -> Result<Option<PathBuf>> {
    Ok(load_db()?
        .entries
        .into_iter()
        .find(|entry| entry.state == CacheEntryState::Ready)
        .map(|entry| PathBuf::from(encrypted_blob_uri(&entry.id))))
}

fn create_prefix_value(
    handle: &NativeModelHandle,
    tier: CacheTier,
    owner_id: Option<String>,
    label: String,
    stable_messages: &[ChatMessage],
) -> Result<PrefixCacheValue> {
    let branches = ["alpha", "beta"]
        .into_iter()
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
            chat_template: ChatTemplateChoice::ModelDefault,
            branches,
            cached_prefix: None,
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
                template: ChatTemplateChoice::ModelDefault,
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

fn persist_value(settings: &Settings, value: &PrefixCacheValue) -> Result<()> {
    persist_value_to_store(
        &RuntimeStore::open(&settings.data_dir)?,
        value,
        DEFAULT_PERSISTENT_CACHE_ENTRIES,
        DEFAULT_PERSISTENT_CACHE_BYTES,
    )
}

fn persist_value_to_store(
    store: &RuntimeStore,
    value: &PrefixCacheValue,
    max_entries: usize,
    max_bytes: usize,
) -> Result<()> {
    if !value.is_valid() {
        return Err(anyhow!(
            "refusing to persist an invalid native prefix cache"
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
            let replaced_ids = db
                .entries
                .iter()
                .filter(|entry| {
                    entry.id == value.metadata.id
                        || (entry.tier == value.metadata.tier
                            && entry.owner_id == value.metadata.owner_id
                            && entry.fingerprint == value.metadata.fingerprint)
                })
                .map(|entry| entry.id.clone())
                .collect::<Vec<_>>();
            for id in replaced_ids {
                documents.delete(&blob_namespace(&id));
            }
            documents.put_bytes(&blob_namespace(&value.metadata.id), &value.sequence.bytes)?;
            db.entries.retain(|entry| {
                entry.id != value.metadata.id
                    && !(entry.tier == value.metadata.tier
                        && entry.owner_id == value.metadata.owner_id
                        && entry.fingerprint == value.metadata.fingerprint)
            });
            db.entries.insert(0, value.metadata.clone());
            let evicted =
                persistent_eviction_ids(&db.entries, &value.metadata.id, max_entries, max_bytes);
            for id in &evicted {
                documents.delete(&blob_namespace(id));
            }
            db.entries.retain(|entry| !evicted.contains(&entry.id));
            Ok(())
        },
    )
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
    let loaded = store.get_bytes(&blob_namespace(&metadata.id));
    let value = match loaded {
        Ok(Some(bytes)) => PrefixCacheValue {
            metadata: metadata.clone(),
            sequence: SequenceStateBlob {
                sequence_id: 0,
                token_count: metadata.token_ids.len(),
                bytes,
                token_ids: metadata.token_ids.clone(),
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
            }
            documents.delete(&blob_namespace(cache_id));
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

fn promote_to_memory(value: PrefixCacheValue) {
    if let Ok(mut cache) = memory_cache().lock() {
        cache.insert(value);
    }
}

fn invalidate_memory(cache_id: &str) {
    if let Ok(mut cache) = memory_cache().lock() {
        cache.invalidate(cache_id);
    }
}

fn memory_totals() -> (usize, usize, usize) {
    memory_cache()
        .lock()
        .map(|cache| (cache.len(), cache.used_bytes(), cache.capacity_bytes()))
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

fn encrypted_blob_uri(id: &str) -> String {
    format!("encrypted://{}", blob_namespace(id))
}

#[cfg(test)]
mod tests {
    use super::*;

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
                bytes: vec![1],
                token_ids: vec![1],
            },
        }
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
        let mut cache = memory_cache().lock().expect("memory cache lock");
        cache.clear();
        cache.insert(test_cache_value("model-a", "fingerprint-a"));
        cache.insert(test_cache_value("model-b", "fingerprint-b"));
        assert_eq!(cache.len(), 2);
        assert_eq!(cache.used_bytes(), 2);
        assert_eq!(cache.capacity_bytes(), DEFAULT_MEMORY_CACHE_BYTES);
        cache.clear();
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
    fn authenticated_persistent_cache_corruption_invalidates_and_falls_back() -> Result<()> {
        let data_dir =
            std::env::temp_dir().join(format!("mom-llama-cache-corruption-{}", crate::now_ms()));
        let store = RuntimeStore::open_with_key(&data_dir, [17_u8; 32])?;
        let mut value = test_cache_value("tampered", "fingerprint");
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
        let connection = rusqlite::Connection::open(store.path())?;
        connection.execute(
            "UPDATE encrypted_documents SET ciphertext = X'00' WHERE namespace = ?1",
            [blob_namespace(&value.metadata.id)],
        )?;

        assert!(load_persistent_value_or_invalidate_from_store(&store, &value.metadata)?.is_none());
        let db = store
            .get::<KvCacheDb>(KV_CACHE_NAMESPACE)?
            .ok_or_else(|| anyhow!("cache metadata missing"))?;
        assert_eq!(db.entries[0].state, CacheEntryState::Invalidated);
        assert_eq!(store.get_bytes(&blob_namespace(&value.metadata.id))?, None);

        let mut missing = test_cache_value("missing", "fingerprint");
        missing.metadata.tier = CacheTier::SessionPersistent;
        store.mutate(KV_CACHE_NAMESPACE, KvCacheDb::default, |db| {
            db.entries.push(missing.metadata.clone());
            Ok(())
        })?;
        assert!(
            load_persistent_value_or_invalidate_from_store(&store, &missing.metadata)?.is_none()
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
            value.sequence.bytes = vec![7; 4];
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
        oversized.sequence.bytes = vec![1; 4];
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
