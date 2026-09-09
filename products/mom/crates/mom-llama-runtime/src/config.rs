use crate::engine::ValidationBlocker;
use crate::receipts::{Blocker, CommandResult};
use crate::store::RuntimeStore;
use anyhow::{Context, Result};
use llama_native_types::{NativeDevice, SamplerKind, SamplingConfig};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use sysinfo::{MemoryRefreshKind, RefreshKind, System};

const SETTINGS_FILE: &str = "settings.json";
pub(crate) const SETTINGS_NAMESPACE: &str = "settings.v2";
const DEFAULT_MAX_TOKENS: u32 = 512;
const GIB: u64 = 1024 * 1024 * 1024;
const FALLBACK_RESIDENT_MEMORY_BUDGET_BYTES: u64 = 8 * GIB;
const MIN_AUTO_RESIDENT_MEMORY_BUDGET_BYTES: u64 = 2 * GIB;
const MAX_AUTO_RESIDENT_MEMORY_BUDGET_BYTES: u64 = 64 * GIB;
const MEMORY_BUDGET_MODE_KEY: &str = "nativeMemoryBudgetMode";
const COMPILED_DEFAULT_MODEL_PATH: Option<&str> = option_env!("MOM_LLAMA_DEFAULT_MODEL_PATH");
const COMPILED_DEFAULT_MMPROJ_PATH: Option<&str> = option_env!("MOM_LLAMA_DEFAULT_MMPROJ_PATH");

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum KvCachePolicy {
    None,
    PromptPrefix,
    KvCacheCandidate,
}

impl KvCachePolicy {
    #[must_use]
    pub const fn allows_prefix_reuse(self) -> bool {
        !matches!(self, Self::None)
    }

    #[must_use]
    pub const fn persists_conversation_checkpoints(self) -> bool {
        matches!(self, Self::KvCacheCandidate)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GenerationDefaults {
    pub temperature: f32,
    pub top_p: f32,
    pub max_tokens: u32,
}

impl Default for GenerationDefaults {
    fn default() -> Self {
        Self {
            temperature: 0.7,
            top_p: 0.95,
            max_tokens: DEFAULT_MAX_TOKENS,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Settings {
    pub model_path: Option<PathBuf>,
    #[serde(default)]
    pub mmproj_path: Option<PathBuf>,
    pub data_dir: PathBuf,
    pub default_temperature: f32,
    pub default_top_p: f32,
    pub default_max_tokens: u32,
    #[serde(default = "default_kv_cache_policy")]
    pub kv_cache_policy: KvCachePolicy,
    pub theme: Option<String>,
    #[serde(default)]
    pub native_device: NativeDevice,
    #[serde(default = "default_context_tokens")]
    pub context_tokens: u32,
    #[serde(default = "default_batch_tokens")]
    pub batch_tokens: u32,
    #[serde(default = "default_parallel_sequences")]
    pub max_parallel_sequences: u32,
    #[serde(default = "default_resident_memory_budget_bytes")]
    pub resident_memory_budget_bytes: u64,
    #[serde(default = "upstream_settings_defaults")]
    pub upstream_settings: BTreeMap<String, Value>,
}

#[derive(Debug, Deserialize)]
struct LegacySettings {
    engine_bin: Option<PathBuf>,
    selected_model: Option<PathBuf>,
    default_max_tokens: Option<u32>,
    kv_cache_policy: Option<String>,
}

impl LegacySettings {
    fn looks_like_legacy(&self) -> bool {
        self.engine_bin.is_some() || self.selected_model.is_some()
    }

    fn into_settings(self, data_dir: PathBuf) -> Settings {
        let mut settings = Settings::defaults_for_data_dir(data_dir);
        settings.model_path = self.selected_model;
        if let Some(max_tokens) = self.default_max_tokens {
            settings.default_max_tokens = max_tokens;
        }
        let policy = match self.kv_cache_policy.as_deref() {
            Some("none") => KvCachePolicy::None,
            Some("prompt_prefix") => KvCachePolicy::PromptPrefix,
            Some("kv_cache_candidate") => KvCachePolicy::KvCacheCandidate,
            _ => KvCachePolicy::KvCacheCandidate,
        };
        set_cache_policy(&mut settings, policy);
        settings
    }
}

impl Settings {
    pub fn defaults_for_data_dir(data_dir: PathBuf) -> Self {
        let defaults = GenerationDefaults::default();
        let resident_memory_budget_bytes = default_resident_memory_budget_bytes();
        let mut upstream_settings = upstream_settings_defaults();
        write_resident_memory_budget_projection(
            &mut upstream_settings,
            ResidentMemoryBudgetMode::Auto,
            resident_memory_budget_bytes,
        );
        Self {
            model_path: None,
            mmproj_path: None,
            data_dir,
            default_temperature: defaults.temperature,
            default_top_p: defaults.top_p,
            default_max_tokens: defaults.max_tokens,
            kv_cache_policy: KvCachePolicy::KvCacheCandidate,
            theme: None,
            native_device: NativeDevice::Auto,
            context_tokens: default_context_tokens(),
            batch_tokens: default_batch_tokens(),
            max_parallel_sequences: default_parallel_sequences(),
            resident_memory_budget_bytes,
            upstream_settings,
        }
    }

    pub fn generation_defaults(&self) -> GenerationDefaults {
        GenerationDefaults {
            temperature: self.default_temperature,
            top_p: self.default_top_p,
            max_tokens: self.default_max_tokens,
        }
    }

    pub fn sampling_config(&self) -> SamplingConfig {
        let mut sampling = SamplingConfig {
            seed: upstream_setting_i64(self, "seed")
                .and_then(|value| u32::try_from(value).ok())
                .unwrap_or(u32::MAX),
            temperature: upstream_setting_f32(self, "temperature")
                .unwrap_or(self.default_temperature),
            dynamic_temperature_range: upstream_setting_f32(self, "dynatemp_range").unwrap_or(0.0),
            dynamic_temperature_exponent: upstream_setting_f32(self, "dynatemp_exponent")
                .unwrap_or(1.0),
            top_k: upstream_setting_i64(self, "top_k")
                .and_then(|value| i32::try_from(value).ok())
                .unwrap_or(40),
            top_p: upstream_setting_f32(self, "top_p").unwrap_or(self.default_top_p),
            min_p: upstream_setting_f32(self, "min_p").unwrap_or(0.0),
            typical_p: upstream_setting_f32(self, "typ_p").unwrap_or(1.0),
            xtc_probability: upstream_setting_f32(self, "xtc_probability").unwrap_or(0.0),
            xtc_threshold: upstream_setting_f32(self, "xtc_threshold").unwrap_or(0.1),
            repeat_last_n: upstream_setting_i64(self, "repeat_last_n")
                .and_then(|value| i32::try_from(value).ok())
                .unwrap_or(64),
            repeat_penalty: upstream_setting_f32(self, "repeat_penalty").unwrap_or(1.0),
            frequency_penalty: upstream_setting_f32(self, "frequency_penalty").unwrap_or(0.0),
            presence_penalty: upstream_setting_f32(self, "presence_penalty").unwrap_or(0.0),
            dry_multiplier: upstream_setting_f32(self, "dry_multiplier").unwrap_or(0.0),
            dry_base: upstream_setting_f32(self, "dry_base").unwrap_or(1.75),
            dry_allowed_length: upstream_setting_i64(self, "dry_allowed_length")
                .and_then(|value| i32::try_from(value).ok())
                .unwrap_or(2),
            dry_penalty_last_n: upstream_setting_i64(self, "dry_penalty_last_n")
                .and_then(|value| i32::try_from(value).ok())
                .unwrap_or(-1),
            sampler_order: sampler_order(self),
            max_tokens: upstream_setting_i64(self, "max_tokens")
                .and_then(|value| u32::try_from(value).ok())
                .filter(|value| *value > 0)
                .unwrap_or(self.default_max_tokens),
            stop: Vec::new(),
        };
        apply_custom_json_sampling(self, &mut sampling);
        sampling
    }
}

fn default_kv_cache_policy() -> KvCachePolicy {
    KvCachePolicy::KvCacheCandidate
}

pub const UPSTREAM_SETTING_KEYS: &[&str] = &[
    "theme",
    "apiKey",
    "systemMessage",
    "pasteLongTextToFileLen",
    "copyTextAttachmentsAsPlainText",
    "sendOnEnter",
    "enableContinueGeneration",
    "pdfAsImage",
    "titleGenerationUseFirstLine",
    "titleGenerationUseLLM",
    "titleGenerationPrompt",
    "maxImageMPixels",
    "showMessageStats",
    "showAgenticTurnStats",
    "showThoughtInProgress",
    "autoMicOnEmpty",
    "renderUserContentAsMarkdown",
    "disableAutoScroll",
    "alwaysShowSidebarOnDesktop",
    "fullHeightCodeBlocks",
    "showRawModelNames",
    "showModelQuantization",
    "showModelTags",
    "showBuildVersion",
    "showSystemMessage",
    "renderThinkingAsMarkdown",
    "temperature",
    "dynatemp_range",
    "dynatemp_exponent",
    "top_k",
    "top_p",
    "min_p",
    "xtc_probability",
    "xtc_threshold",
    "typ_p",
    "max_tokens",
    "samplers",
    "backend_sampling",
    "repeat_last_n",
    "repeat_penalty",
    "presence_penalty",
    "frequency_penalty",
    "dry_multiplier",
    "dry_base",
    "dry_allowed_length",
    "dry_penalty_last_n",
    "mcpServers",
    "mcpRequestTimeoutSeconds",
    "agenticMaxTurns",
    "alwaysShowToolCallContent",
    "preEncodeConversation",
    "disableReasoningParsing",
    "excludeReasoningFromContext",
    "showRawOutputSwitch",
    "jsSandboxEnabled",
    "symbolicMathEnabled",
    "customJson",
    "customCss",
];

pub const NATIVE_SETTING_EXTENSION_KEYS: &[&str] = &[
    "agenticMaxToolPreviewLines",
    "lookupCacheEnabled",
    "mcpNativeEnabled",
    "mmprojPath",
    "nativeBatchTokens",
    "nativeContextTokens",
    "nativeDevice",
    "nativeMemoryBudgetMiB",
    "nativeModelSlots",
];

const CUSTOM_JSON_SAMPLING_KEYS: &[&str] = &[
    "temperature",
    "dynatemp_range",
    "dynatemp_exponent",
    "top_k",
    "top_p",
    "min_p",
    "xtc_probability",
    "xtc_threshold",
    "typ_p",
    "max_tokens",
    "repeat_last_n",
    "repeat_penalty",
    "presence_penalty",
    "frequency_penalty",
    "dry_multiplier",
    "dry_base",
    "dry_allowed_length",
    "dry_penalty_last_n",
];

fn sampler_order(settings: &Settings) -> Vec<SamplerKind> {
    let Some(value) = settings
        .upstream_settings
        .get("samplers")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    else {
        return SamplingConfig::default().sampler_order;
    };
    let parsed = value
        .split([',', ';', ' '])
        .filter_map(|name| match name.trim().to_ascii_lowercase().as_str() {
            "penalties" => Some(SamplerKind::Penalties),
            "dry" => Some(SamplerKind::Dry),
            "top_k" | "top-k" => Some(SamplerKind::TopK),
            "typ_p" | "typical" | "typical_p" => Some(SamplerKind::TypicalP),
            "top_p" | "top-p" => Some(SamplerKind::TopP),
            "min_p" | "min-p" => Some(SamplerKind::MinP),
            "xtc" => Some(SamplerKind::Xtc),
            "temperature" | "temp" => Some(SamplerKind::Temperature),
            _ => None,
        })
        .collect::<Vec<_>>();
    if parsed.is_empty() {
        SamplingConfig::default().sampler_order
    } else {
        parsed
    }
}

#[derive(Debug, Clone, Default)]
pub struct SettingsUpdate {
    /// `None` leaves the setting unchanged; `Some(None)` clears it.
    pub model_path: Option<Option<PathBuf>>,
    /// `None` leaves the setting unchanged; `Some(None)` clears it.
    pub mmproj_path: Option<Option<PathBuf>>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub max_tokens: Option<u32>,
    pub kv_cache_policy: Option<KvCachePolicy>,
    pub upstream_settings: Option<BTreeMap<String, Value>>,
    pub native_device: Option<NativeDevice>,
    pub context_tokens: Option<u32>,
    pub batch_tokens: Option<u32>,
    pub max_parallel_sequences: Option<u32>,
    pub resident_memory_budget_bytes: Option<u64>,
}

const fn default_context_tokens() -> u32 {
    8192
}

const fn default_batch_tokens() -> u32 {
    512
}

const fn default_parallel_sequences() -> u32 {
    4
}

fn default_resident_memory_budget_bytes() -> u64 {
    automatic_resident_memory_budget(physical_memory_bytes())
}

fn physical_memory_bytes() -> Option<u64> {
    static PHYSICAL_MEMORY: OnceLock<Option<u64>> = OnceLock::new();
    *PHYSICAL_MEMORY.get_or_init(|| {
        let system = System::new_with_specifics(
            RefreshKind::nothing().with_memory(MemoryRefreshKind::nothing().with_ram()),
        );
        let bytes = system.total_memory();
        (bytes > 0).then_some(bytes)
    })
}

fn automatic_resident_memory_budget(physical_memory_bytes: Option<u64>) -> u64 {
    match physical_memory_bytes {
        Some(bytes) => (bytes / 2).clamp(
            MIN_AUTO_RESIDENT_MEMORY_BUDGET_BYTES,
            MAX_AUTO_RESIDENT_MEMORY_BUDGET_BYTES,
        ),
        None => FALLBACK_RESIDENT_MEMORY_BUDGET_BYTES,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResidentMemoryBudgetMode {
    Auto,
    Manual,
}

fn reconcile_resident_memory_budget(settings: &mut Settings, physical_memory: Option<u64>) {
    let declared_mode = settings
        .upstream_settings
        .get(MEMORY_BUDGET_MODE_KEY)
        .and_then(Value::as_str);
    let legacy_budget_mib = upstream_setting_i64(settings, "nativeMemoryBudgetMiB")
        .and_then(|value| u64::try_from(value).ok());
    let mode = match declared_mode {
        Some("manual") => ResidentMemoryBudgetMode::Manual,
        Some("auto") => ResidentMemoryBudgetMode::Auto,
        _ if legacy_budget_mib.is_some_and(|value| value != 8192) => {
            ResidentMemoryBudgetMode::Manual
        }
        // The historical default and an explicit 8192 MiB choice are
        // indistinguishable in documents written before mode provenance. The
        // migration treats that exact legacy value as auto; every subsequent
        // explicit update records `manual` and is preserved exactly.
        _ => ResidentMemoryBudgetMode::Auto,
    };
    if mode == ResidentMemoryBudgetMode::Auto {
        settings.resident_memory_budget_bytes = automatic_resident_memory_budget(physical_memory);
    } else {
        settings.resident_memory_budget_bytes =
            settings.resident_memory_budget_bytes.max(256 * 1024 * 1024);
    }
    let bytes = settings.resident_memory_budget_bytes;
    write_resident_memory_budget_projection(&mut settings.upstream_settings, mode, bytes);
}

pub(crate) fn reconcile_resident_memory_budget_for_runtime(settings: &mut Settings) {
    reconcile_resident_memory_budget(settings, physical_memory_bytes());
}

fn write_resident_memory_budget_projection(
    upstream_settings: &mut BTreeMap<String, Value>,
    mode: ResidentMemoryBudgetMode,
    bytes: u64,
) {
    upstream_settings.insert(
        MEMORY_BUDGET_MODE_KEY.to_string(),
        json!(match mode {
            ResidentMemoryBudgetMode::Auto => "auto",
            ResidentMemoryBudgetMode::Manual => "manual",
        }),
    );
    upstream_settings.insert(
        "nativeMemoryBudgetMiB".to_string(),
        json!(bytes / (1024 * 1024)),
    );
}

pub fn upstream_settings_defaults() -> BTreeMap<String, Value> {
    BTreeMap::from([
        ("theme".to_string(), json!("system")),
        ("apiKey".to_string(), json!("")),
        ("systemMessage".to_string(), json!("")),
        ("pasteLongTextToFileLen".to_string(), json!(2500)),
        ("copyTextAttachmentsAsPlainText".to_string(), json!(false)),
        ("sendOnEnter".to_string(), json!(true)),
        ("enableContinueGeneration".to_string(), json!(false)),
        ("pdfAsImage".to_string(), json!(false)),
        ("titleGenerationUseFirstLine".to_string(), json!(true)),
        ("titleGenerationUseLLM".to_string(), json!(false)),
        (
            "titleGenerationPrompt".to_string(),
            json!(
                "Generate a concise conversation title from the first user and assistant exchange."
            ),
        ),
        ("maxImageMPixels".to_string(), json!(0)),
        ("showMessageStats".to_string(), json!(false)),
        ("showAgenticTurnStats".to_string(), json!(false)),
        ("showThoughtInProgress".to_string(), json!(true)),
        ("alwaysShowToolCallContent".to_string(), json!(false)),
        ("autoMicOnEmpty".to_string(), json!(false)),
        ("renderUserContentAsMarkdown".to_string(), json!(false)),
        ("fullHeightCodeBlocks".to_string(), json!(false)),
        ("disableAutoScroll".to_string(), json!(false)),
        ("alwaysShowSidebarOnDesktop".to_string(), json!(false)),
        ("showRawModelNames".to_string(), json!(false)),
        ("showModelQuantization".to_string(), json!(true)),
        ("showModelTags".to_string(), json!(true)),
        ("showBuildVersion".to_string(), json!(false)),
        ("showSystemMessage".to_string(), json!(true)),
        ("renderThinkingAsMarkdown".to_string(), json!(true)),
        ("temperature".to_string(), json!(0.7)),
        ("dynatemp_range".to_string(), Value::Null),
        ("dynatemp_exponent".to_string(), Value::Null),
        ("top_k".to_string(), Value::Null),
        ("top_p".to_string(), json!(0.95)),
        ("min_p".to_string(), Value::Null),
        ("xtc_probability".to_string(), Value::Null),
        ("xtc_threshold".to_string(), Value::Null),
        ("typ_p".to_string(), Value::Null),
        ("max_tokens".to_string(), json!(DEFAULT_MAX_TOKENS)),
        ("samplers".to_string(), json!("")),
        ("backend_sampling".to_string(), json!(false)),
        ("repeat_last_n".to_string(), Value::Null),
        ("repeat_penalty".to_string(), Value::Null),
        ("presence_penalty".to_string(), Value::Null),
        ("frequency_penalty".to_string(), Value::Null),
        ("dry_multiplier".to_string(), Value::Null),
        ("dry_base".to_string(), Value::Null),
        ("dry_allowed_length".to_string(), Value::Null),
        ("dry_penalty_last_n".to_string(), Value::Null),
        ("agenticMaxTurns".to_string(), json!(8)),
        ("preEncodeConversation".to_string(), json!(true)),
        ("disableReasoningParsing".to_string(), json!(false)),
        ("excludeReasoningFromContext".to_string(), json!(false)),
        ("showRawOutputSwitch".to_string(), json!(false)),
        ("jsSandboxEnabled".to_string(), json!(false)),
        ("symbolicMathEnabled".to_string(), json!(false)),
        ("customJson".to_string(), json!("")),
        ("customCss".to_string(), json!("")),
        ("mcpRequestTimeoutSeconds".to_string(), json!(30)),
        ("mcpServers".to_string(), json!("[]")),
        // Native-only settings remain in the same encrypted map for backwards
        // compatibility, but are not counted as upstream settings parity.
        ("agenticMaxToolPreviewLines".to_string(), json!(25)),
        ("mcpNativeEnabled".to_string(), json!(false)),
        ("mmprojPath".to_string(), json!("")),
        ("nativeModelSlots".to_string(), json!(1)),
        ("nativeMemoryBudgetMiB".to_string(), json!(8192)),
        (MEMORY_BUDGET_MODE_KEY.to_string(), json!("auto")),
        ("nativeDevice".to_string(), json!("auto")),
        ("nativeContextTokens".to_string(), json!(8192)),
        ("nativeBatchTokens".to_string(), json!(512)),
        ("lookupCacheEnabled".to_string(), json!(false)),
    ])
}

pub fn upstream_setting_f32(settings: &Settings, key: &str) -> Option<f32> {
    match settings.upstream_settings.get(key) {
        Some(Value::Number(value)) => value.as_f64().map(|value| value as f32),
        Some(Value::String(value)) => value.parse::<f32>().ok(),
        _ => None,
    }
}

pub fn upstream_setting_i64(settings: &Settings, key: &str) -> Option<i64> {
    match settings.upstream_settings.get(key) {
        Some(Value::Number(value)) => value
            .as_i64()
            .or_else(|| value.as_u64().map(|value| value as i64)),
        Some(Value::String(value)) => value.parse::<i64>().ok(),
        _ => None,
    }
}

pub fn upstream_setting_string(settings: &Settings, key: &str) -> Option<String> {
    match settings.upstream_settings.get(key) {
        Some(Value::String(value)) if !value.is_empty() => Some(value.clone()),
        Some(Value::Number(value)) => Some(value.to_string()),
        Some(Value::Bool(value)) => Some(value.to_string()),
        _ => None,
    }
}

pub fn resolve_settings() -> Result<Settings> {
    let data_dir = resolve_data_dir();
    fs::create_dir_all(&data_dir)
        .with_context(|| format!("failed to create data dir {}", data_dir.display()))?;
    let path = data_dir.join(SETTINGS_FILE);
    let store = RuntimeStore::open(&data_dir)?;
    let settings = if let Some(settings) = store.get::<Settings>(SETTINGS_NAMESPACE)? {
        settings
    } else if path.exists() {
        let (migrated, legacy_engine_path) = read_settings_file(&path, data_dir.clone())?;
        store.put(SETTINGS_NAMESPACE, &migrated)?;
        let migration_id = format!("mom_llama.settings_migration:{}", crate::now_ms());
        store.write_receipt(
            &migration_id,
            "mom_llama.settings_migrate",
            &json!({
                "schema": "mom_llama.settings_migration.v1",
                "status": "migrated",
                "source": path,
                "native_backend": true,
                "legacy_engine_path_ignored": legacy_engine_path,
            }),
        )?;
        migrated
    } else {
        Settings::defaults_for_data_dir(data_dir.clone())
    };
    settings_from_document(data_dir, Some(settings)).map_err(anyhow::Error::new)
}

pub(crate) fn settings_from_document(
    data_dir: PathBuf,
    stored: Option<Settings>,
) -> std::result::Result<Settings, ValidationBlocker> {
    settings_from_document_with_model_sources(
        data_dir,
        stored,
        runtime_model_override(),
        COMPILED_DEFAULT_MODEL_PATH,
        COMPILED_DEFAULT_MMPROJ_PATH,
    )
}

pub(crate) fn runtime_model_override() -> Option<PathBuf> {
    std::env::var_os("MOM_LLAMA_MODEL_PATH")
        .map(PathBuf::from)
        .and_then(|path| normalize_optional_path(Some(path)))
}

fn settings_from_document_with_model_sources(
    data_dir: PathBuf,
    stored: Option<Settings>,
    runtime_model: Option<PathBuf>,
    compiled_model: Option<&str>,
    compiled_mmproj: Option<&str>,
) -> std::result::Result<Settings, ValidationBlocker> {
    let mut settings = stored.unwrap_or_else(|| Settings::defaults_for_data_dir(data_dir.clone()));
    settings.data_dir = data_dir;
    settings.model_path = normalize_optional_path(settings.model_path);
    settings.mmproj_path = normalize_optional_path(settings.mmproj_path);
    reconcile_resident_memory_budget_for_runtime(&mut settings);
    merge_missing_setting_defaults(&mut settings);
    let cache_policy = settings.kv_cache_policy;
    set_cache_policy(&mut settings, cache_policy);
    if let Some(model) = normalize_optional_path(runtime_model) {
        // The explicit runtime model remains the highest-precedence source,
        // but it is still one atomic model/projector pair. Never inherit a
        // persisted projector from a different model.
        settings.mmproj_path = crate::models::discover_projector_for_model(&model)?;
        settings.model_path = Some(model);
    } else if settings.model_path.is_none()
        && let Some(model) = compiled_default_path(compiled_model, CompiledPathKind::Model)?
    {
        settings.mmproj_path =
            match compiled_default_path(compiled_mmproj, CompiledPathKind::Projector)? {
                Some(projector) => Some(projector),
                None => crate::models::discover_projector_for_model(&model)?,
            };
        settings.model_path = Some(model);
    }
    settings.upstream_settings.insert(
        "mmprojPath".to_string(),
        json!(
            settings
                .mmproj_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_default()
        ),
    );
    Ok(settings)
}

pub(crate) fn normalize_optional_path(path: Option<PathBuf>) -> Option<PathBuf> {
    path.filter(|path| {
        !path.as_os_str().is_empty() && path.to_str().is_none_or(|value| !value.trim().is_empty())
    })
}

#[derive(Debug, Clone, Copy)]
enum CompiledPathKind {
    Model,
    Projector,
}

fn compiled_default_path(
    value: Option<&str>,
    kind: CompiledPathKind,
) -> std::result::Result<Option<PathBuf>, ValidationBlocker> {
    let Some(path) = normalize_optional_path(value.map(PathBuf::from)) else {
        return Ok(None);
    };
    // Relative compile-time paths depend on the launch working directory and
    // can silently become `.`. A set-and-forget build default must be exact.
    if path.is_absolute() {
        return Ok(Some(path));
    }
    let (readiness, code, noun) = match kind {
        CompiledPathKind::Model => (
            "blocked_invalid_model",
            "compiled_default_model_path_relative",
            "model",
        ),
        CompiledPathKind::Projector => (
            "blocked_invalid_mmproj",
            "compiled_default_mmproj_path_relative",
            "vision projector",
        ),
    };
    Err(ValidationBlocker {
        readiness: readiness.to_string(),
        blocker: Blocker::new(
            code,
            format!("The compiled default {noun} path is not absolute."),
            vec![format!(
                "Rebuild with an absolute MOM_LLAMA_DEFAULT_{} path.",
                match kind {
                    CompiledPathKind::Model => "MODEL_PATH",
                    CompiledPathKind::Projector => "MMPROJ_PATH",
                }
            )],
        ),
    })
}

fn read_settings_file(path: &Path, data_dir: PathBuf) -> Result<(Settings, Option<PathBuf>)> {
    let raw = fs::read_to_string(path)?;
    let legacy_engine_path = serde_json::from_str::<Value>(&raw).ok().and_then(|value| {
        value
            .get("engine_path")
            .or_else(|| value.get("engine_bin"))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    });
    match serde_json::from_str::<Settings>(&raw) {
        Ok(settings) => Ok((settings, legacy_engine_path)),
        Err(settings_error) => {
            if let Ok(legacy) = serde_json::from_str::<LegacySettings>(&raw)
                && legacy.looks_like_legacy()
            {
                return Ok((legacy.into_settings(data_dir), legacy_engine_path));
            }
            Err(settings_error).with_context(|| format!("failed to parse {}", path.display()))
        }
    }
}

pub fn save_settings(settings: &Settings) -> Result<PathBuf> {
    fs::create_dir_all(&settings.data_dir)?;
    let store = RuntimeStore::open(&settings.data_dir)?;
    store.put(SETTINGS_NAMESPACE, settings)?;
    Ok(store.path().to_path_buf())
}

pub fn settings_get() -> Result<CommandResult<Settings>> {
    let settings = resolve_settings()?;
    Ok(CommandResult::passed(
        "mom_llama.settings_get",
        "contracted",
        settings,
        Vec::new(),
        Vec::new(),
        false,
        false,
    ))
}

pub fn configure_engine(
    model_path: PathBuf,
    native_device: Option<NativeDevice>,
    context_tokens: Option<u32>,
    batch_tokens: Option<u32>,
    max_parallel_sequences: Option<u32>,
    resident_memory_budget_bytes: Option<u64>,
) -> Result<CommandResult<Settings>> {
    let mut result = settings_update(SettingsUpdate {
        model_path: Some(Some(model_path)),
        native_device,
        context_tokens,
        batch_tokens,
        max_parallel_sequences,
        resident_memory_budget_bytes,
        ..SettingsUpdate::default()
    })?;
    result.command = "mom_llama.engine_configure".to_string();
    result.receipt.command = result.command.clone();
    Ok(result)
}

pub fn settings_update(update: SettingsUpdate) -> Result<CommandResult<Settings>> {
    let mut settings = resolve_settings()?;
    let projector_path_explicit = update.mmproj_path.is_some();
    let requested_cache_policy = update.kv_cache_policy;
    let requested_preencode = update
        .upstream_settings
        .as_ref()
        .and_then(|values| values.get("preEncodeConversation"))
        .and_then(Value::as_bool);
    let requested_memory_budget = update
        .upstream_settings
        .as_ref()
        .is_some_and(|values| values.contains_key("nativeMemoryBudgetMiB"));
    if let Some(model_path) = update.model_path {
        settings.model_path = normalize_optional_path(model_path);
        if !projector_path_explicit {
            settings.mmproj_path = None;
            settings
                .upstream_settings
                .insert("mmprojPath".to_string(), json!(""));
        }
    }
    if let Some(mmproj_path) = update.mmproj_path {
        settings.mmproj_path = normalize_optional_path(mmproj_path);
        settings.upstream_settings.insert(
            "mmprojPath".to_string(),
            json!(
                settings
                    .mmproj_path
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default()
            ),
        );
    }
    if let Some(temperature) = update.temperature {
        settings.default_temperature = temperature;
        settings
            .upstream_settings
            .insert("temperature".to_string(), json!(temperature));
    }
    if let Some(top_p) = update.top_p {
        settings.default_top_p = top_p;
        settings
            .upstream_settings
            .insert("top_p".to_string(), json!(top_p));
    }
    if let Some(max_tokens) = update.max_tokens {
        settings.default_max_tokens = max_tokens;
        settings
            .upstream_settings
            .insert("max_tokens".to_string(), json!(max_tokens));
    }
    if let Some(native_device) = update.native_device {
        settings.native_device = native_device;
        settings.upstream_settings.insert(
            "nativeDevice".to_string(),
            json!(format!("{native_device:?}").to_lowercase()),
        );
    }
    if let Some(context_tokens) = update.context_tokens {
        settings.context_tokens = context_tokens.max(512);
        settings.upstream_settings.insert(
            "nativeContextTokens".to_string(),
            json!(settings.context_tokens),
        );
    }
    if let Some(batch_tokens) = update.batch_tokens {
        settings.batch_tokens = batch_tokens.max(1);
        settings.upstream_settings.insert(
            "nativeBatchTokens".to_string(),
            json!(settings.batch_tokens),
        );
    }
    if let Some(max_parallel_sequences) = update.max_parallel_sequences {
        settings.max_parallel_sequences = max_parallel_sequences.clamp(1, 4);
        settings.upstream_settings.insert(
            "nativeModelSlots".to_string(),
            json!(settings.max_parallel_sequences),
        );
    }
    if let Some(resident_memory_budget_bytes) = update.resident_memory_budget_bytes {
        settings.resident_memory_budget_bytes = resident_memory_budget_bytes.max(256 * 1024 * 1024);
        let bytes = settings.resident_memory_budget_bytes;
        write_resident_memory_budget_projection(
            &mut settings.upstream_settings,
            ResidentMemoryBudgetMode::Manual,
            bytes,
        );
    }
    if let Some(upstream_settings) = update.upstream_settings {
        if let Some(blocker) = validate_settings_update(&upstream_settings) {
            return Ok(CommandResult::blocked(
                "mom_llama.settings_update",
                "stub_blocked",
                blocker,
            ));
        }
        for (key, value) in upstream_settings {
            settings.upstream_settings.insert(key, value);
        }
        sync_generation_defaults_from_upstream(&mut settings);
        sync_native_defaults_from_upstream(&mut settings);
        if requested_memory_budget {
            let bytes = settings.resident_memory_budget_bytes;
            write_resident_memory_budget_projection(
                &mut settings.upstream_settings,
                ResidentMemoryBudgetMode::Manual,
                bytes,
            );
        }
    }
    if let Some(policy) = requested_cache_policy.or_else(|| {
        requested_preencode.map(|enabled| {
            if enabled {
                KvCachePolicy::KvCacheCandidate
            } else {
                KvCachePolicy::PromptPrefix
            }
        })
    }) {
        set_cache_policy(&mut settings, policy);
    }
    if let Some(theme) = settings
        .upstream_settings
        .get("theme")
        .and_then(Value::as_str)
    {
        settings.theme = Some(theme.to_string());
    }
    let path = save_settings(&settings)?;
    Ok(CommandResult::passed(
        "mom_llama.settings_update",
        "contracted",
        settings,
        vec![path.display().to_string()],
        Vec::new(),
        false,
        false,
    ))
}

fn set_cache_policy(settings: &mut Settings, policy: KvCachePolicy) {
    settings.kv_cache_policy = policy;
    settings.upstream_settings.insert(
        "preEncodeConversation".to_string(),
        json!(policy.persists_conversation_checkpoints()),
    );
}

fn merge_missing_setting_defaults(settings: &mut Settings) {
    for (key, value) in upstream_settings_defaults() {
        settings.upstream_settings.entry(key).or_insert(value);
    }
}

fn validate_settings_update(values: &BTreeMap<String, Value>) -> Option<Blocker> {
    for (key, value) in values {
        if !UPSTREAM_SETTING_KEYS.contains(&key.as_str())
            && !NATIVE_SETTING_EXTENSION_KEYS.contains(&key.as_str())
        {
            return Some(Blocker::new(
                "setting_key_unknown",
                format!("`{key}` is not a current upstream or native setting."),
                vec!["Use a setting exposed by the native Settings screen.".to_string()],
            ));
        }
        if key == "customJson" {
            let Some(raw) = value.as_str() else {
                return Some(Blocker::new(
                    "custom_json_invalid",
                    "Custom JSON must be a JSON object encoded as text.",
                    vec!["Enter an object such as `{\"temperature\":0.5}`.".to_string()],
                ));
            };
            if raw.trim().is_empty() {
                continue;
            }
            let Ok(Value::Object(custom)) = serde_json::from_str::<Value>(raw) else {
                return Some(Blocker::new(
                    "custom_json_invalid",
                    "Custom JSON must contain one valid JSON object.",
                    vec!["Correct the JSON and try again.".to_string()],
                ));
            };
            if let Some(unknown) = custom
                .keys()
                .find(|candidate| !CUSTOM_JSON_SAMPLING_KEYS.contains(&candidate.as_str()))
            {
                return Some(Blocker::new(
                    "custom_json_key_not_allowlisted",
                    format!("Custom JSON key `{unknown}` is not supported by the native sampler."),
                    vec![format!(
                        "Use one of: {}.",
                        CUSTOM_JSON_SAMPLING_KEYS.join(", ")
                    )],
                ));
            }
        }
        if key == "customCss" {
            let Some(css) = value.as_str() else {
                return Some(Blocker::new(
                    "custom_css_invalid",
                    "Custom CSS must be text.",
                    vec!["Enter local CSS declarations only.".to_string()],
                ));
            };
            let lower = css.to_ascii_lowercase();
            if lower.contains("@import") || lower.contains("url(") {
                return Some(Blocker::new(
                    "custom_css_external_resource_blocked",
                    "Custom CSS cannot import or load external resources in the native-local profile.",
                    vec!["Remove @import and url(...) references.".to_string()],
                ));
            }
        }
    }
    None
}

fn apply_custom_json_sampling(settings: &Settings, sampling: &mut SamplingConfig) {
    let Some(Value::String(raw)) = settings.upstream_settings.get("customJson") else {
        return;
    };
    let Ok(Value::Object(custom)) = serde_json::from_str::<Value>(raw) else {
        return;
    };
    let f32_value = |key: &str| {
        custom
            .get(key)
            .and_then(Value::as_f64)
            .map(|value| value as f32)
    };
    let i64_value = |key: &str| custom.get(key).and_then(Value::as_i64);
    if let Some(value) = f32_value("temperature") {
        sampling.temperature = value;
    }
    if let Some(value) = f32_value("dynatemp_range") {
        sampling.dynamic_temperature_range = value;
    }
    if let Some(value) = f32_value("dynatemp_exponent") {
        sampling.dynamic_temperature_exponent = value;
    }
    if let Some(value) = i64_value("top_k").and_then(|value| i32::try_from(value).ok()) {
        sampling.top_k = value;
    }
    if let Some(value) = f32_value("top_p") {
        sampling.top_p = value;
    }
    if let Some(value) = f32_value("min_p") {
        sampling.min_p = value;
    }
    if let Some(value) = f32_value("xtc_probability") {
        sampling.xtc_probability = value;
    }
    if let Some(value) = f32_value("xtc_threshold") {
        sampling.xtc_threshold = value;
    }
    if let Some(value) = f32_value("typ_p") {
        sampling.typical_p = value;
    }
    if let Some(value) = i64_value("max_tokens")
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
    {
        sampling.max_tokens = value;
    }
    if let Some(value) = i64_value("repeat_last_n").and_then(|value| i32::try_from(value).ok()) {
        sampling.repeat_last_n = value;
    }
    if let Some(value) = f32_value("repeat_penalty") {
        sampling.repeat_penalty = value;
    }
    if let Some(value) = f32_value("presence_penalty") {
        sampling.presence_penalty = value;
    }
    if let Some(value) = f32_value("frequency_penalty") {
        sampling.frequency_penalty = value;
    }
    if let Some(value) = f32_value("dry_multiplier") {
        sampling.dry_multiplier = value;
    }
    if let Some(value) = f32_value("dry_base") {
        sampling.dry_base = value;
    }
    if let Some(value) = i64_value("dry_allowed_length").and_then(|value| i32::try_from(value).ok())
    {
        sampling.dry_allowed_length = value;
    }
    if let Some(value) = i64_value("dry_penalty_last_n").and_then(|value| i32::try_from(value).ok())
    {
        sampling.dry_penalty_last_n = value;
    }
}

fn sync_generation_defaults_from_upstream(settings: &mut Settings) {
    if let Some(temperature) = upstream_setting_f32(settings, "temperature") {
        settings.default_temperature = temperature;
    }
    if let Some(top_p) = upstream_setting_f32(settings, "top_p") {
        settings.default_top_p = top_p;
    }
    if let Some(max_tokens) = upstream_setting_i64(settings, "max_tokens")
        && max_tokens > 0
    {
        settings.default_max_tokens = max_tokens as u32;
    }
}

fn sync_native_defaults_from_upstream(settings: &mut Settings) {
    settings.mmproj_path = settings
        .upstream_settings
        .get("mmprojPath")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    settings.native_device = match settings
        .upstream_settings
        .get("nativeDevice")
        .and_then(Value::as_str)
    {
        Some("cpu") => NativeDevice::Cpu,
        Some("metal") => NativeDevice::Metal,
        _ => NativeDevice::Auto,
    };
    if let Some(value) = upstream_setting_i64(settings, "nativeContextTokens")
        && let Ok(value) = u32::try_from(value)
    {
        settings.context_tokens = value.max(512);
    }
    if let Some(value) = upstream_setting_i64(settings, "nativeBatchTokens")
        && let Ok(value) = u32::try_from(value)
    {
        settings.batch_tokens = value.max(1);
    }
    if let Some(value) = upstream_setting_i64(settings, "nativeModelSlots")
        && let Ok(value) = u32::try_from(value)
    {
        settings.max_parallel_sequences = value.clamp(1, 4);
    }
    if let Some(value) = upstream_setting_i64(settings, "nativeMemoryBudgetMiB")
        && let Ok(value) = u64::try_from(value)
    {
        settings.resident_memory_budget_bytes =
            value.saturating_mul(1024 * 1024).max(256 * 1024 * 1024);
    }
}

pub fn settings_reset() -> Result<CommandResult<Settings>> {
    let current = resolve_settings()?;
    let settings = Settings::defaults_for_data_dir(current.data_dir.clone());
    let path = save_settings(&settings)?;
    Ok(CommandResult::passed(
        "mom_llama.settings_reset",
        "contracted",
        settings,
        vec![path.display().to_string()],
        Vec::new(),
        false,
        false,
    ))
}

pub fn resolve_data_dir() -> PathBuf {
    if let Some(path) = data_dir_override() {
        return path;
    }
    if let Ok(path) = std::env::var("LLAMA_NATIVE_KIT_DATA_DIR")
        && !path.is_empty()
    {
        return PathBuf::from(path);
    }
    if let Ok(home) = std::env::var("HOME")
        && !home.is_empty()
    {
        return PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("llama-native-kit")
            .join(if insecure_development_store_enabled() {
                "mom-llama-development"
            } else {
                "mom-llama"
            });
    }
    std::env::temp_dir()
        .join("llama-native-kit")
        .join(if insecure_development_store_enabled() {
            "mom-llama-development"
        } else {
            "mom-llama"
        })
}

pub(crate) fn insecure_development_store_enabled() -> bool {
    cfg!(debug_assertions)
        && !std::env::var("LLAMA_NATIVE_KIT_SECURE_STORAGE")
            .ok()
            .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "yes"))
}

pub fn set_data_dir_override_for_tests(path: Option<PathBuf>) {
    DATA_DIR_OVERRIDE.with(|override_path| *override_path.borrow_mut() = path);
}

std::thread_local! {
    // A test override belongs to the test thread that installed it. Making it
    // process-global lets unrelated parallel tests open and mutate the same
    // SQLite fixture while its owning test is using it.
    static DATA_DIR_OVERRIDE: std::cell::RefCell<Option<PathBuf>> = const {
        std::cell::RefCell::new(None)
    };
}

fn data_dir_override() -> Option<PathBuf> {
    DATA_DIR_OVERRIDE.with(|override_path| override_path.borrow().clone())
}

pub fn write_json_atomic<T>(path: &Path, value: &T) -> Result<()>
where
    T: Serialize,
{
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, serde_json::to_string_pretty(value)?)?;
    fs::rename(tmp, path)?;
    Ok(())
}

pub fn read_json_or_default<T>(path: &Path) -> Result<T>
where
    T: for<'de> Deserialize<'de> + Default,
{
    if !path.exists() {
        return Ok(T::default());
    }
    serde_json::from_str(&fs::read_to_string(path)?)
        .with_context(|| format!("failed to parse {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_dir_override_does_not_leak_into_parallel_tests() {
        let override_path = std::env::temp_dir().join(format!(
            "mom-llama-config-thread-override-{}",
            uuid::Uuid::new_v4()
        ));
        set_data_dir_override_for_tests(Some(override_path.clone()));

        let parallel_data_dir = std::thread::spawn(resolve_data_dir).join();
        set_data_dir_override_for_tests(None);
        let parallel_data_dir = parallel_data_dir.expect("parallel data-dir lookup");
        assert_ne!(parallel_data_dir, override_path);
    }

    #[test]
    fn exact_upstream_setting_registry_is_complete_and_distinct_from_extensions() {
        let defaults = upstream_settings_defaults();
        assert_eq!(UPSTREAM_SETTING_KEYS.len(), 58);
        for key in UPSTREAM_SETTING_KEYS {
            assert!(
                defaults.contains_key(*key),
                "missing upstream default {key}"
            );
            assert!(!NATIVE_SETTING_EXTENSION_KEYS.contains(key));
        }
        for key in NATIVE_SETTING_EXTENSION_KEYS {
            assert!(
                defaults.contains_key(*key),
                "missing native extension {key}"
            );
        }
    }

    #[test]
    fn cache_defaults_are_automatic_and_policy_is_the_single_source_of_truth() {
        let mut settings = Settings::defaults_for_data_dir(std::env::temp_dir());
        assert_eq!(settings.kv_cache_policy, KvCachePolicy::KvCacheCandidate);
        assert_eq!(
            settings.upstream_settings.get("preEncodeConversation"),
            Some(&json!(true))
        );

        for (policy, checkpoint) in [
            (KvCachePolicy::None, false),
            (KvCachePolicy::PromptPrefix, false),
            (KvCachePolicy::KvCacheCandidate, true),
        ] {
            set_cache_policy(&mut settings, policy);
            assert_eq!(settings.kv_cache_policy, policy);
            assert_eq!(
                settings.upstream_settings.get("preEncodeConversation"),
                Some(&json!(checkpoint))
            );
            assert_eq!(policy.allows_prefix_reuse(), policy != KvCachePolicy::None);
            assert_eq!(policy.persists_conversation_checkpoints(), checkpoint);
        }
    }

    #[test]
    fn generation_defaults_leave_room_for_reasoning_models_to_answer() {
        let settings = Settings::defaults_for_data_dir(std::env::temp_dir());
        assert_eq!(settings.default_max_tokens, DEFAULT_MAX_TOKENS);
        assert_eq!(settings.sampling_config().max_tokens, DEFAULT_MAX_TOKENS);
    }

    #[test]
    fn automatic_memory_budget_is_half_of_ram_with_conservative_bounds() {
        assert_eq!(
            automatic_resident_memory_budget(None),
            FALLBACK_RESIDENT_MEMORY_BUDGET_BYTES
        );
        assert_eq!(automatic_resident_memory_budget(Some(GIB)), 2 * GIB);
        assert_eq!(automatic_resident_memory_budget(Some(16 * GIB)), 8 * GIB);
        assert_eq!(automatic_resident_memory_budget(Some(128 * GIB)), 64 * GIB);
        assert_eq!(automatic_resident_memory_budget(Some(512 * GIB)), 64 * GIB);
    }

    #[test]
    fn automatic_and_manual_memory_budget_provenance_survives_document_resolution() {
        let mut manual = Settings::defaults_for_data_dir(std::env::temp_dir());
        manual.resident_memory_budget_bytes = 12 * GIB;
        write_resident_memory_budget_projection(
            &mut manual.upstream_settings,
            ResidentMemoryBudgetMode::Manual,
            12 * GIB,
        );
        let resolved = settings_from_document_with_model_sources(
            std::env::temp_dir(),
            Some(manual),
            None,
            None,
            None,
        )
        .expect("manual memory budget");
        assert_eq!(resolved.resident_memory_budget_bytes, 12 * GIB);
        assert_eq!(
            resolved.upstream_settings.get(MEMORY_BUDGET_MODE_KEY),
            Some(&json!("manual"))
        );

        let mut legacy_non_default = Settings::defaults_for_data_dir(std::env::temp_dir());
        legacy_non_default.resident_memory_budget_bytes = 12 * GIB;
        legacy_non_default
            .upstream_settings
            .remove(MEMORY_BUDGET_MODE_KEY);
        legacy_non_default
            .upstream_settings
            .insert("nativeMemoryBudgetMiB".to_string(), json!(12 * 1024));
        reconcile_resident_memory_budget(&mut legacy_non_default, Some(128 * GIB));
        assert_eq!(legacy_non_default.resident_memory_budget_bytes, 12 * GIB);
        assert_eq!(
            legacy_non_default
                .upstream_settings
                .get(MEMORY_BUDGET_MODE_KEY),
            Some(&json!("manual"))
        );

        let mut legacy_default = Settings::defaults_for_data_dir(std::env::temp_dir());
        legacy_default.resident_memory_budget_bytes = 8 * GIB;
        legacy_default
            .upstream_settings
            .remove(MEMORY_BUDGET_MODE_KEY);
        legacy_default
            .upstream_settings
            .insert("nativeMemoryBudgetMiB".to_string(), json!(8192));
        reconcile_resident_memory_budget(&mut legacy_default, Some(128 * GIB));
        assert_eq!(legacy_default.resident_memory_budget_bytes, 64 * GIB);
        assert_eq!(
            legacy_default.upstream_settings.get(MEMORY_BUDGET_MODE_KEY),
            Some(&json!("auto"))
        );
    }

    #[test]
    fn empty_paths_never_become_dot_and_compile_defaults_must_be_absolute() {
        assert_eq!(normalize_optional_path(Some(PathBuf::new())), None);
        assert_eq!(normalize_optional_path(Some(PathBuf::from("   "))), None);
        assert_eq!(
            compiled_default_path(None, CompiledPathKind::Model).expect("no compiled default"),
            None
        );
        assert_eq!(
            compiled_default_path(Some(""), CompiledPathKind::Model)
                .expect("empty compiled default"),
            None
        );
        let relative = compiled_default_path(Some("relative/model.gguf"), CompiledPathKind::Model)
            .expect_err("relative compiled model must fail closed");
        assert_eq!(
            relative.blocker.code,
            "compiled_default_model_path_relative"
        );
        let absolute_model = std::env::temp_dir()
            .join("mom-llama-compiled-defaults")
            .join("default.gguf");
        assert!(
            absolute_model.is_absolute(),
            "the host temporary directory must provide an absolute test path"
        );
        assert_eq!(
            compiled_default_path(
                Some(
                    absolute_model
                        .to_str()
                        .expect("temporary path must be valid Unicode"),
                ),
                CompiledPathKind::Model,
            )
            .expect("absolute compiled default"),
            Some(absolute_model)
        );
    }

    #[test]
    fn runtime_and_compiled_models_resolve_one_projector_or_propagate_typed_ambiguity() {
        let temporary = std::env::temp_dir().join(format!(
            "mom-llama-config-model-pair-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&temporary).expect("model pair directory");
        let model = temporary.join("vision-model.gguf");
        let projector = temporary.join("mmproj-F16.gguf");
        std::fs::write(&model, b"GGUF").expect("model");
        std::fs::write(&projector, b"GGUF").expect("projector");

        let runtime = settings_from_document_with_model_sources(
            temporary.join("runtime-data"),
            None,
            Some(model.clone()),
            None,
            None,
        )
        .expect("runtime model pair");
        assert_eq!(runtime.model_path, Some(model.clone()));
        assert_eq!(runtime.mmproj_path, Some(projector.clone()));

        let model_text = model.to_str().expect("UTF-8 model path");
        let compiled = settings_from_document_with_model_sources(
            temporary.join("compiled-data"),
            None,
            None,
            Some(model_text),
            None,
        )
        .expect("compiled model pair");
        assert_eq!(compiled.model_path, Some(model.clone()));
        assert_eq!(compiled.mmproj_path, Some(projector));

        std::fs::write(temporary.join("mmproj-BF16.gguf"), b"GGUF").expect("second projector");
        for (runtime_model, compiled_model) in
            [(Some(model.clone()), None), (None, Some(model_text))]
        {
            let blocked = settings_from_document_with_model_sources(
                temporary.join("ambiguous-data"),
                None,
                runtime_model,
                compiled_model,
                None,
            )
            .expect_err("ambiguous automatic pairing must remain typed");
            assert_eq!(blocked.readiness, "blocked_ambiguous_projector");
            assert_eq!(blocked.blocker.code, "mmproj_path_ambiguous");
        }

        std::fs::remove_file(temporary.join("mmproj-F16.gguf")).expect("remove projector");
        std::fs::remove_file(temporary.join("mmproj-BF16.gguf")).expect("remove second projector");
        for index in 0..256 {
            std::fs::write(temporary.join(format!("note-{index:03}.txt")), b"x")
                .expect("bounded sibling");
        }
        for (runtime_model, compiled_model) in
            [(Some(model.clone()), None), (None, Some(model_text))]
        {
            let blocked = settings_from_document_with_model_sources(
                temporary.join("bounded-data"),
                None,
                runtime_model,
                compiled_model,
                None,
            )
            .expect_err("bounded automatic pairing must remain typed");
            assert_eq!(blocked.readiness, "blocked_projector_scan_bound");
            assert_eq!(blocked.blocker.code, "projector_directory_too_large");
        }

        std::fs::remove_dir_all(&temporary).expect("remove model pair directory");
    }

    #[test]
    fn custom_json_is_allowlisted_and_changes_native_sampling() {
        let mut settings = Settings::defaults_for_data_dir(std::env::temp_dir());
        settings.upstream_settings.insert(
            "customJson".to_string(),
            json!(r#"{"temperature":0.25,"top_k":7,"max_tokens":33}"#),
        );
        assert!(
            validate_settings_update(&BTreeMap::from([(
                "customJson".to_string(),
                settings.upstream_settings["customJson"].clone(),
            )]))
            .is_none()
        );
        let sampling = settings.sampling_config();
        assert_eq!(sampling.temperature, 0.25);
        assert_eq!(sampling.top_k, 7);
        assert_eq!(sampling.max_tokens, 33);

        let blocker = validate_settings_update(&BTreeMap::from([(
            "customJson".to_string(),
            json!(r#"{"arbitrary_shell_flag":true}"#),
        )]))
        .expect("unknown custom JSON authority must block");
        assert_eq!(blocker.code, "custom_json_key_not_allowlisted");
    }

    #[test]
    fn custom_css_cannot_load_external_resources() {
        let blocker = validate_settings_update(&BTreeMap::from([(
            "customCss".to_string(),
            json!(format!(
                ".app {{ background: url({}{}); }}",
                "https", "://example.invalid/pixel"
            )),
        )]))
        .expect("external CSS resources must block");
        assert_eq!(blocker.code, "custom_css_external_resource_blocked");
        assert!(
            validate_settings_update(&BTreeMap::from([(
                "customCss".to_string(),
                json!(".message-card { line-height: 1.7; }"),
            )]))
            .is_none()
        );
    }
}
