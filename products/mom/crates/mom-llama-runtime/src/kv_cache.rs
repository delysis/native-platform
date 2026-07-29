use crate::config::{KvCachePolicy, resolve_settings};
use crate::native_runtime::resident_model;
use crate::now_ms;
use crate::receipts::{Blocker, CommandResult};
use crate::skill_store::load_skill_db;
use crate::store::RuntimeStore;
use anyhow::Result;
use llama_native_engine::LLAMA_CPP_BINDING_VERSION;
use llama_native_types::{
    BranchRequest, ChatMessage, ChatRole, GenerationRequest, GenerationState, SamplingConfig,
    SequenceStateBlob, SharedPrefixBatchRequest,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use uuid::Uuid;

const KV_CACHE_FILE: &str = "kv-cache.json";
const KV_CACHE_NAMESPACE: &str = "kv-cache.v2";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum KvCacheState {
    UnsupportedByEngine,
    BlockedMissingModel,
    BlockedMissingCacheDir,
    ConfiguredNotVerified,
    PromptSmokeVerified,
    Saved,
    Restored,
    Invalidated,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KvCacheMetadata {
    pub id: String,
    pub skill_id: Option<String>,
    pub model_path: Option<String>,
    pub model_hash: Option<String>,
    pub prompt_hash: Option<String>,
    pub cache_path: PathBuf,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub status: KvCacheState,
    #[serde(default)]
    pub token_count: usize,
    #[serde(default)]
    pub binding_version: String,
    #[serde(default)]
    pub build_id: String,
    #[serde(default)]
    pub tokenizer_hash: String,
    #[serde(default)]
    pub template_hash: String,
    #[serde(default)]
    pub multimodal_projector_hash: Option<String>,
    #[serde(default)]
    pub context_tokens: u32,
    #[serde(default)]
    pub batch_tokens: u32,
    #[serde(default)]
    pub max_sequences: u32,
    #[serde(default)]
    pub native_device: String,
    #[serde(default)]
    pub token_ids: Vec<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct KvCacheDb {
    #[serde(default)]
    pub entries: Vec<KvCacheMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KvCacheStatus {
    pub status: KvCacheState,
    pub policy: KvCachePolicy,
    pub cache_dir: String,
    pub entries: Vec<KvCacheMetadata>,
}

pub fn kv_cache_status() -> Result<CommandResult<KvCacheStatus>> {
    let settings = resolve_settings()?;
    let db = load_db()?;
    let cache_dir = settings.data_dir.join("runtime.sqlite3");
    let status = if settings.model_path.is_none() {
        KvCacheState::BlockedMissingModel
    } else if db
        .entries
        .iter()
        .any(|entry| matches!(entry.status, KvCacheState::Saved | KvCacheState::Restored))
    {
        KvCacheState::Saved
    } else if matches!(settings.kv_cache_policy, KvCachePolicy::None) {
        KvCacheState::UnsupportedByEngine
    } else {
        KvCacheState::ConfiguredNotVerified
    };
    Ok(CommandResult::passed(
        "mom_llama.kv_cache_status",
        "contracted",
        KvCacheStatus {
            status,
            policy: settings.kv_cache_policy,
            cache_dir: cache_dir.display().to_string(),
            entries: db.entries,
        },
        Vec::new(),
        Vec::new(),
        false,
        false,
    ))
}

pub fn kv_cache_save(skill_id: Option<String>) -> Result<CommandResult<KvCacheMetadata>> {
    let settings = resolve_settings()?;
    if matches!(settings.kv_cache_policy, KvCachePolicy::None) {
        return Ok(CommandResult::blocked(
            "mom_llama.kv_cache_save",
            "stub_blocked",
            Blocker::new(
                "kv_cache_policy_disabled",
                "KV/prompt cache policy is disabled.",
                vec!["Enable prompt-prefix or KV-cache-candidate policy.".to_string()],
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
    let prompt = cache_prompt(skill_id.as_deref())?;
    let prompt_hash = sha256_string(&prompt);
    let status = handle.status();
    let state = handle
        .prefill_shared_prefix(SharedPrefixBatchRequest {
            request_id: format!("kv-cache-smoke-{}", Uuid::new_v4()),
            model_id: status.model_id.clone(),
            common_messages: vec![ChatMessage {
                role: ChatRole::System,
                content: prompt.clone(),
            }],
            branches: vec![
                BranchRequest {
                    branch_id: "cache-probe-a".to_string(),
                    label: "Cache probe A".to_string(),
                    instruction: "alpha cache probe".to_string(),
                    sampling: SamplingConfig::default(),
                },
                BranchRequest {
                    branch_id: "cache-probe-b".to_string(),
                    label: "Cache probe B".to_string(),
                    instruction: "beta cache probe".to_string(),
                    sampling: SamplingConfig::default(),
                },
            ],
            cached_prefix: None,
        })
        .map_err(|error| anyhow::anyhow!(error))?;
    let id = Uuid::new_v4().to_string();
    let blob_namespace = format!("kv-cache.blob.{id}");
    let store = RuntimeStore::open(&settings.data_dir)?;
    store.put_bytes(&blob_namespace, &state.bytes)?;
    let cache_path = PathBuf::from(format!("encrypted://{blob_namespace}"));
    let fingerprint = status.fingerprint;
    let metadata = KvCacheMetadata {
        id,
        skill_id,
        model_path: settings
            .model_path
            .as_ref()
            .map(|path| path.display().to_string()),
        model_hash: fingerprint.as_ref().map(|value| value.model_sha256.clone()),
        prompt_hash: Some(prompt_hash),
        cache_path: cache_path.clone(),
        created_at: now_ms().to_string(),
        last_used_at: None,
        status: KvCacheState::Saved,
        token_count: state.token_count,
        binding_version: LLAMA_CPP_BINDING_VERSION.to_string(),
        build_id: fingerprint
            .as_ref()
            .map(|value| value.build_id.clone())
            .unwrap_or_default(),
        tokenizer_hash: fingerprint
            .as_ref()
            .map(|value| value.tokenizer_sha256.clone())
            .unwrap_or_default(),
        template_hash: fingerprint
            .as_ref()
            .map(|value| value.chat_template_sha256.clone())
            .unwrap_or_default(),
        multimodal_projector_hash: current_mmproj_hash(&settings)?,
        context_tokens: settings.context_tokens,
        batch_tokens: settings.batch_tokens,
        max_sequences: settings.max_parallel_sequences,
        native_device: format!("{:?}", settings.native_device).to_lowercase(),
        token_ids: state.token_ids,
    };
    let mut db = load_db()?;
    db.entries
        .retain(|entry| entry.prompt_hash != metadata.prompt_hash);
    db.entries.insert(0, metadata.clone());
    let db_path = save_db(&db)?;
    Ok(CommandResult::passed(
        "mom_llama.kv_cache_save",
        "prompt_smoke_verified",
        metadata,
        vec![
            cache_path.display().to_string(),
            db_path.display().to_string(),
        ],
        Vec::new(),
        true,
        false,
    ))
}

pub fn kv_cache_restore(cache_id: Option<String>) -> Result<CommandResult<KvCacheMetadata>> {
    let settings = resolve_settings()?;
    let mut db = load_db()?;
    let Some(index) = cache_id
        .as_deref()
        .and_then(|id| db.entries.iter().position(|entry| entry.id == id))
        .or_else(|| (!db.entries.is_empty()).then_some(0))
    else {
        return Ok(CommandResult::blocked(
            "mom_llama.kv_cache_restore",
            "stub_blocked",
            Blocker::new(
                "kv_cache_not_found",
                "No saved prompt cache was found.",
                vec!["Save a prompt cache first.".to_string()],
            ),
        ));
    };
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
    let status = handle.status();
    let entry = &db.entries[index];
    let compatible = entry.binding_version == LLAMA_CPP_BINDING_VERSION
        && entry.context_tokens == settings.context_tokens
        && entry.batch_tokens == settings.batch_tokens
        && entry.max_sequences == settings.max_parallel_sequences
        && entry.native_device == format!("{:?}", settings.native_device).to_lowercase()
        && entry.model_hash
            == status
                .fingerprint
                .as_ref()
                .map(|fingerprint| fingerprint.model_sha256.clone());
    let fingerprint_compatible = status.fingerprint.as_ref().is_some_and(|fingerprint| {
        entry.build_id == fingerprint.build_id
            && entry.tokenizer_hash == fingerprint.tokenizer_sha256
            && entry.template_hash == fingerprint.chat_template_sha256
    });
    let prompt_compatible =
        entry.prompt_hash == Some(sha256_string(&cache_prompt(entry.skill_id.as_deref())?));
    let projector_compatible = entry.multimodal_projector_hash == current_mmproj_hash(&settings)?;
    let blob_namespace = format!("kv-cache.blob.{}", entry.id);
    let store = RuntimeStore::open(&settings.data_dir)?;
    let state_bytes = store.get_bytes(&blob_namespace)?;
    if !compatible
        || !fingerprint_compatible
        || !prompt_compatible
        || !projector_compatible
        || state_bytes.is_none()
    {
        db.entries[index].status = KvCacheState::Invalidated;
        let _ = save_db(&db)?;
        return Ok(CommandResult::blocked(
            "mom_llama.kv_cache_restore",
            "stub_blocked",
            Blocker::new(
                "kv_cache_incompatible",
                "The saved cache does not match the resident model and native context.",
                vec!["Save a new cache for this model.".to_string()],
            ),
        ));
    }
    let bytes = state_bytes.unwrap_or_default();
    let state = SequenceStateBlob {
        sequence_id: 0,
        token_count: entry.token_count,
        bytes,
        token_ids: entry.token_ids.clone(),
    };
    let prompt = cache_prompt(entry.skill_id.as_deref())?;
    if !verify_restore_equivalence(&handle, &prompt, &state)? {
        db.entries[index].status = KvCacheState::Invalidated;
        save_db(&db)?;
        return Ok(CommandResult::blocked(
            "mom_llama.kv_cache_restore",
            "stub_blocked",
            Blocker::new(
                "kv_cache_restore_mismatch",
                "The restored native sequence did not reproduce the uncached deterministic continuation.",
                vec!["Save a new cache for this model and prompt.".to_string()],
            ),
        ));
    }
    handle
        .restore_sequence(state, 0)
        .map_err(|error| anyhow::anyhow!(error))?;
    db.entries[index].status = KvCacheState::Restored;
    db.entries[index].last_used_at = Some(now_ms().to_string());
    let metadata = db.entries[index].clone();
    let db_path = save_db(&db)?;
    Ok(CommandResult::passed(
        "mom_llama.kv_cache_restore",
        "prompt_smoke_verified",
        metadata,
        vec![db_path.display().to_string()],
        Vec::new(),
        true,
        false,
    ))
}

fn verify_restore_equivalence(
    handle: &llama_native_engine::NativeModelHandle,
    prompt: &str,
    state: &SequenceStateBlob,
) -> Result<bool> {
    let request =
        |request_id: String, cached_prefix: Option<SequenceStateBlob>| GenerationRequest {
            request_id,
            model_id: handle.status().model_id,
            messages: vec![
                ChatMessage {
                    role: ChatRole::System,
                    content: prompt.to_string(),
                },
                ChatMessage {
                    role: ChatRole::User,
                    content: "Reply with exactly: cache verified".to_string(),
                },
            ],
            sampling: SamplingConfig {
                seed: 1,
                temperature: 0.0,
                max_tokens: 8,
                ..SamplingConfig::default()
            },
            media: Vec::new(),
            cached_prefix,
        };
    let cached = handle
        .generate(request(
            format!("kv-cache-restore-{}", Uuid::new_v4()),
            Some(state.clone()),
        ))
        .map_err(|error| anyhow::anyhow!(error))?
        .wait()
        .map_err(|error| anyhow::anyhow!(error))?;
    let uncached = handle
        .generate(request(
            format!("kv-cache-baseline-{}", Uuid::new_v4()),
            None,
        ))
        .map_err(|error| anyhow::anyhow!(error))?
        .wait()
        .map_err(|error| anyhow::anyhow!(error))?;
    let cached = cached.first();
    let uncached = uncached.first();
    Ok(
        cached.is_some_and(|output| output.state == GenerationState::Completed)
            && uncached.is_some_and(|output| output.state == GenerationState::Completed)
            && cached.map(|output| output.text.as_str())
                == uncached.map(|output| output.text.as_str()),
    )
}

pub fn kv_cache_clear() -> Result<CommandResult<KvCacheStatus>> {
    let settings = resolve_settings()?;
    let db = load_db()?;
    let store = RuntimeStore::open(&settings.data_dir)?;
    for entry in &db.entries {
        store.delete(&format!("kv-cache.blob.{}", entry.id))?;
    }
    let path = save_db(&KvCacheDb::default())?;
    Ok(CommandResult::passed(
        "mom_llama.kv_cache_clear",
        "contracted",
        KvCacheStatus {
            status: KvCacheState::Invalidated,
            policy: settings.kv_cache_policy,
            cache_dir: "encrypted://runtime.sqlite3".to_string(),
            entries: Vec::new(),
        },
        vec![path.display().to_string()],
        Vec::new(),
        false,
        false,
    ))
}

fn load_db() -> Result<KvCacheDb> {
    let settings = resolve_settings()?;
    let store = RuntimeStore::open(&settings.data_dir)?;
    store.import_json_once::<KvCacheDb>(
        KV_CACHE_NAMESPACE,
        &settings.data_dir.join(KV_CACHE_FILE),
    )?;
    Ok(store.get(KV_CACHE_NAMESPACE)?.unwrap_or_default())
}

fn save_db(db: &KvCacheDb) -> Result<PathBuf> {
    let settings = resolve_settings()?;
    let store = RuntimeStore::open(&settings.data_dir)?;
    store.put(KV_CACHE_NAMESPACE, db)?;
    Ok(store.path().to_path_buf())
}

pub fn latest_verified_cache_path() -> Result<Option<PathBuf>> {
    let db = load_db()?;
    Ok(db
        .entries
        .into_iter()
        .find(|entry| matches!(entry.status, KvCacheState::Saved | KvCacheState::Restored))
        .map(|entry| entry.cache_path))
}

pub fn compatible_cached_prefix(prompt: &str) -> Result<Option<(String, SequenceStateBlob)>> {
    if prompt.trim().is_empty() {
        return Ok(None);
    }
    let settings = resolve_settings()?;
    let handle = match resident_model(&settings) {
        Ok(handle) => handle,
        Err(_) => return Ok(None),
    };
    let status = handle.status();
    let mut db = load_db()?;
    let prompt_hash = sha256_string(prompt);
    let Some(index) = db.entries.iter().position(|entry| {
        entry.prompt_hash.as_deref() == Some(prompt_hash.as_str())
            && matches!(entry.status, KvCacheState::Saved | KvCacheState::Restored)
    }) else {
        return Ok(None);
    };
    let entry = &db.entries[index];
    let compatible = entry.binding_version == LLAMA_CPP_BINDING_VERSION
        && entry.context_tokens == settings.context_tokens
        && entry.batch_tokens == settings.batch_tokens
        && entry.max_sequences == settings.max_parallel_sequences
        && entry.native_device == format!("{:?}", settings.native_device).to_lowercase()
        && status.fingerprint.as_ref().is_some_and(|fingerprint| {
            entry.model_hash.as_deref() == Some(fingerprint.model_sha256.as_str())
                && entry.build_id == fingerprint.build_id
                && entry.tokenizer_hash == fingerprint.tokenizer_sha256
                && entry.template_hash == fingerprint.chat_template_sha256
                && entry.multimodal_projector_hash == fingerprint.multimodal_projector_sha256
        });
    let bytes = RuntimeStore::open(&settings.data_dir)?
        .get_bytes(&format!("kv-cache.blob.{}", entry.id))?;
    if !compatible
        || bytes.is_none()
        || entry.token_count == 0
        || entry.token_count != entry.token_ids.len()
    {
        db.entries[index].status = KvCacheState::Invalidated;
        save_db(&db)?;
        return Ok(None);
    }
    Ok(Some((
        entry.id.clone(),
        SequenceStateBlob {
            sequence_id: 0,
            token_count: entry.token_count,
            bytes: bytes.unwrap_or_default(),
            token_ids: entry.token_ids.clone(),
        },
    )))
}

pub fn invalidate_cache(cache_id: &str) -> Result<()> {
    let mut db = load_db()?;
    if let Some(entry) = db.entries.iter_mut().find(|entry| entry.id == cache_id) {
        entry.status = KvCacheState::Invalidated;
        save_db(&db)?;
    }
    Ok(())
}

fn cache_prompt(skill_id: Option<&str>) -> Result<String> {
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
    Ok("You are a concise, kind local assistant.".to_string())
}

fn sha256_string(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn current_mmproj_hash(settings: &crate::config::Settings) -> Result<Option<String>> {
    let Some(path) = settings.mmproj_path.as_ref() else {
        return Ok(None);
    };
    if !path.is_file() {
        return Ok(None);
    }
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(Some(format!("{:x}", hasher.finalize())))
}
