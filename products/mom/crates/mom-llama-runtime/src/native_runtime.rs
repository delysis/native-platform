use crate::config::Settings;
use crate::engine::{ValidationBlocker, validate_model_path};
use crate::receipts::Blocker;
use llama_native_engine::NativeModelHandle;
use llama_native_types::{NativeError, NativeModelConfig, ResidentModelStatus};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

#[derive(Debug, Default)]
struct ResidentRegistry {
    slots: BTreeMap<usize, ResidentEntry>,
}

#[derive(Debug)]
struct ResidentEntry {
    model_path: PathBuf,
    context_tokens: u32,
    batch_tokens: u32,
    max_sequences: u32,
    handle: NativeModelHandle,
    model_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResidentSlotStatus {
    pub slot_id: usize,
    pub model_path: PathBuf,
    pub model_bytes: u64,
    pub status: ResidentModelStatus,
}

static REGISTRY: OnceLock<Mutex<ResidentRegistry>> = OnceLock::new();

fn registry() -> &'static Mutex<ResidentRegistry> {
    REGISTRY.get_or_init(|| Mutex::new(ResidentRegistry::default()))
}

pub fn resident_model(settings: &Settings) -> Result<NativeModelHandle, ValidationBlocker> {
    resident_model_for_slot(settings, 0, settings.model_path.as_deref())
}

pub fn resident_model_for_slot(
    settings: &Settings,
    slot_id: usize,
    requested_model_path: Option<&Path>,
) -> Result<NativeModelHandle, ValidationBlocker> {
    if slot_id >= settings.max_parallel_sequences.clamp(1, 4) as usize {
        return Err(native_blocker(
            "native_slot_out_of_range",
            "The requested resident model slot is outside the configured bound.",
        ));
    }
    if requested_model_path.is_none()
        && let Ok(registry) = registry().lock()
        && let Some(entry) = registry.slots.get(&slot_id)
    {
        return Ok(entry.handle.clone());
    }
    let Some(model_path) = requested_model_path else {
        return Err(ValidationBlocker {
            readiness: "blocked_missing_model".to_string(),
            blocker: Blocker::new(
                "model_path_missing",
                "No GGUF model path is configured.",
                vec![
                    "Set MOM_LLAMA_MODEL_PATH.".to_string(),
                    "Run `mom-llama model select --model-path ...`.".to_string(),
                ],
            ),
        });
    };
    validate_model_path(model_path)?;
    let mut registry = registry().lock().map_err(|_| {
        native_blocker(
            "native_registry_poisoned",
            "The native model registry is unavailable.",
        )
    })?;
    if let Some(entry) = registry.slots.get(&slot_id)
        && entry.matches(settings, model_path)
    {
        return Ok(entry.handle.clone());
    }
    let model_bytes = std::fs::metadata(model_path)
        .map(|metadata| metadata.len())
        .map_err(|_| {
            native_blocker(
                "model_metadata_unavailable",
                "The selected model size could not be read.",
            )
        })?;
    let current_bytes = registry
        .slots
        .iter()
        .filter(|(candidate, _)| **candidate != slot_id)
        .map(|(_, entry)| entry.model_bytes)
        .sum::<u64>();
    if current_bytes.saturating_add(model_bytes) > settings.resident_memory_budget_bytes {
        return Err(ValidationBlocker {
            readiness: "blocked_memory_budget".to_string(),
            blocker: Blocker::new(
                "resident_model_memory_budget_exceeded",
                format!(
                    "Loading this model would require at least {} bytes across resident models, above the configured {} byte budget.",
                    current_bytes.saturating_add(model_bytes),
                    settings.resident_memory_budget_bytes
                ),
                vec![
                    "Unload another model slot or raise the resident model memory budget."
                        .to_string(),
                ],
            ),
        });
    }
    let mut config = NativeModelConfig::local(model_path.to_path_buf());
    config.device = settings.native_device;
    config.context_tokens = settings.context_tokens;
    config.batch_tokens = settings.batch_tokens;
    config.max_sequences = settings.max_parallel_sequences.clamp(1, 4);
    config.mmproj_path = settings.mmproj_path.clone();
    let handle = NativeModelHandle::load(config).map_err(native_error_blocker)?;
    registry.slots.insert(
        slot_id,
        ResidentEntry {
            model_path: model_path.to_path_buf(),
            context_tokens: settings.context_tokens,
            batch_tokens: settings.batch_tokens,
            max_sequences: settings.max_parallel_sequences.clamp(1, 4),
            handle: handle.clone(),
            model_bytes,
        },
    );
    Ok(handle)
}

pub fn resident_status() -> Option<ResidentModelStatus> {
    registry()
        .lock()
        .ok()
        .and_then(|registry| registry.slots.get(&0).map(|entry| entry.handle.status()))
}

pub fn resident_slots() -> Vec<ResidentSlotStatus> {
    registry()
        .lock()
        .map(|registry| {
            registry
                .slots
                .iter()
                .map(|(slot_id, entry)| ResidentSlotStatus {
                    slot_id: *slot_id,
                    model_path: entry.model_path.clone(),
                    model_bytes: entry.model_bytes,
                    status: entry.handle.status(),
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn unload_resident_slot(slot_id: usize) -> bool {
    registry()
        .lock()
        .map(|mut registry| registry.slots.remove(&slot_id).is_some())
        .unwrap_or(false)
}

pub fn unload_resident_model() -> bool {
    registry()
        .lock()
        .map(|mut registry| {
            let had_models = !registry.slots.is_empty();
            registry.slots.clear();
            had_models
        })
        .unwrap_or(false)
}

pub fn cancel_native_request(request_id: &str, branch_id: Option<&str>) -> usize {
    registry()
        .lock()
        .ok()
        .map(|registry| {
            registry
                .slots
                .values()
                .map(|entry| entry.handle.cancel(request_id, branch_id))
                .sum()
        })
        .unwrap_or_default()
}

impl ResidentEntry {
    fn matches(&self, settings: &Settings, model_path: &Path) -> bool {
        self.model_path == model_path
            && self.context_tokens == settings.context_tokens
            && self.batch_tokens == settings.batch_tokens
            && self.max_sequences == settings.max_parallel_sequences.clamp(1, 4)
    }
}

fn native_error_blocker(error: NativeError) -> ValidationBlocker {
    ValidationBlocker {
        readiness: match error.code {
            llama_native_types::NativeErrorCode::ModelMissing => "blocked_missing_model",
            llama_native_types::NativeErrorCode::ModelInvalid
            | llama_native_types::NativeErrorCode::ModelLoadFailed => "blocked_invalid_model",
            _ => "blocked_native_runtime",
        }
        .to_string(),
        blocker: Blocker::new(
            error.code.to_string(),
            error.message,
            vec!["Check the selected model and native runtime settings.".to_string()],
        ),
    }
}

fn native_blocker(code: &str, message: &str) -> ValidationBlocker {
    ValidationBlocker {
        readiness: "blocked_native_runtime".to_string(),
        blocker: Blocker::new(code, message, vec!["Restart Mom Llama Lab.".to_string()]),
    }
}
