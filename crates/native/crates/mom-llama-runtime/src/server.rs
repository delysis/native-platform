//! Deprecated `server` command aliases over the native resident-model runtime.
//!
//! These functions deliberately contain no socket, HTTP, or process behavior. The
//! legacy command IDs remain temporarily so old automation can discover and
//! control the same in-process workers used by the Tauri application.

use crate::config::{SettingsUpdate, resolve_settings, settings_update};
use crate::engine::validate_model_path;
use crate::native_runtime::{
    resident_model, resident_model_for_slot, resident_slots, unload_resident_model,
    unload_resident_slot,
};
use crate::receipts::CommandResult;
use anyhow::Result;
use llama_native_types::NativeDevice;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerConfig {
    pub model_path: Option<PathBuf>,
    pub device: NativeDevice,
    pub context_tokens: u32,
    pub batch_tokens: u32,
    pub max_sequences: u32,
    pub resident_memory_budget_bytes: u64,
    pub transport: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServerStatus {
    pub configured: bool,
    pub running: bool,
    pub transport: String,
    pub resident_models: usize,
    pub active_sequences: usize,
    pub resident_model_bytes: u64,
    pub memory_budget_bytes: u64,
    pub health: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelSlot {
    pub slot_id: usize,
    pub model_path: Option<String>,
    pub model_bytes: u64,
    pub active_sequences: usize,
    pub max_sequences: u32,
    pub state: String,
    pub transport: String,
}

pub fn server_configure(
    model_path: Option<PathBuf>,
    slots: Option<u32>,
    memory_budget_bytes: Option<u64>,
) -> Result<CommandResult<ServerConfig>> {
    if let Some(model_path) = model_path.as_ref()
        && let Err(blocked) = validate_model_path(model_path)
    {
        return Ok(CommandResult::blocked(
            "mom_llama.server_configure",
            &blocked.readiness,
            blocked.blocker,
        ));
    }
    let settings = settings_update(SettingsUpdate {
        model_path,
        max_parallel_sequences: slots,
        resident_memory_budget_bytes: memory_budget_bytes,
        ..SettingsUpdate::default()
    })?
    .result
    .unwrap_or(resolve_settings()?);
    Ok(CommandResult::passed(
        "mom_llama.server_configure",
        "contracted",
        config_from_settings(&settings),
        Vec::new(),
        vec![
            "Deprecated alias: configuration applies to the in-process resident-model runtime."
                .to_string(),
        ],
        false,
        false,
    ))
}

pub fn server_status() -> Result<CommandResult<ServerStatus>> {
    let settings = resolve_settings()?;
    let slots = resident_slots();
    let active_sequences = slots.iter().map(|slot| slot.status.active_sequences).sum();
    let resident_model_bytes = slots.iter().map(|slot| slot.model_bytes).sum();
    Ok(CommandResult::passed(
        "mom_llama.server_status",
        if slots.is_empty() {
            "contracted"
        } else {
            "host_integrated"
        },
        ServerStatus {
            configured: settings.model_path.is_some(),
            running: !slots.is_empty(),
            transport: "in_process".to_string(),
            resident_models: slots.len(),
            active_sequences,
            resident_model_bytes,
            memory_budget_bytes: settings.resident_memory_budget_bytes,
            health: Some(json!({
                "state": if slots.is_empty() { "unloaded" } else { "ready" },
                "binding": llama_native_engine::LLAMA_CPP_BINDING_VERSION,
            })),
        },
        Vec::new(),
        vec!["Deprecated alias: no server was launched or contacted.".to_string()],
        false,
        false,
    ))
}

pub fn server_start() -> Result<CommandResult<ServerStatus>> {
    let settings = resolve_settings()?;
    let handle = match resident_model(&settings) {
        Ok(handle) => handle,
        Err(blocked) => {
            return Ok(CommandResult::blocked(
                "mom_llama.server_start",
                &blocked.readiness,
                blocked.blocker,
            ));
        }
    };
    let status = handle.status();
    Ok(CommandResult::passed(
        "mom_llama.server_start",
        "host_integrated",
        ServerStatus {
            configured: true,
            running: true,
            transport: "in_process".to_string(),
            resident_models: resident_slots().len(),
            active_sequences: status.active_sequences,
            resident_model_bytes: resident_slots().iter().map(|slot| slot.model_bytes).sum(),
            memory_budget_bytes: settings.resident_memory_budget_bytes,
            health: Some(json!({
                "state": status.state,
                "model_id": status.model_id,
                "max_sequences": status.max_sequences,
            })),
        },
        Vec::new(),
        vec![
            "Deprecated alias: loaded a native resident model; no server was launched.".to_string(),
        ],
        false,
        false,
    ))
}

pub fn server_stop() -> Result<CommandResult<ServerStatus>> {
    let settings = resolve_settings()?;
    unload_resident_model();
    Ok(CommandResult::passed(
        "mom_llama.server_stop",
        "contracted",
        ServerStatus {
            configured: settings.model_path.is_some(),
            running: false,
            transport: "in_process".to_string(),
            resident_models: 0,
            active_sequences: 0,
            resident_model_bytes: 0,
            memory_budget_bytes: settings.resident_memory_budget_bytes,
            health: Some(json!({"state": "unloaded"})),
        },
        Vec::new(),
        vec![
            "Deprecated alias: unloaded native resident models; no server was stopped.".to_string(),
        ],
        false,
        false,
    ))
}

pub fn model_slot_list() -> Result<CommandResult<Vec<ModelSlot>>> {
    let slots = resident_slots()
        .into_iter()
        .map(|slot| ModelSlot {
            slot_id: slot.slot_id,
            model_path: Some(slot.model_path.display().to_string()),
            model_bytes: slot.model_bytes,
            active_sequences: slot.status.active_sequences,
            max_sequences: slot.status.max_sequences,
            state: format!("{:?}", slot.status.state).to_lowercase(),
            transport: "in_process".to_string(),
        })
        .collect::<Vec<_>>();
    Ok(CommandResult::passed(
        "mom_llama.model_slot_list",
        if slots.is_empty() {
            "contracted"
        } else {
            "host_integrated"
        },
        slots,
        Vec::new(),
        Vec::new(),
        false,
        false,
    ))
}

pub fn model_slot_load(slot_id: usize, model_path: PathBuf) -> Result<CommandResult<ModelSlot>> {
    if let Err(blocked) = validate_model_path(&model_path) {
        return Ok(CommandResult::blocked(
            "mom_llama.model_slot_load",
            &blocked.readiness,
            blocked.blocker,
        ));
    }
    let mut settings = resolve_settings()?;
    if slot_id == 0 && settings.model_path.as_ref() != Some(&model_path) {
        settings = settings_update(SettingsUpdate {
            model_path: Some(model_path.clone()),
            ..SettingsUpdate::default()
        })?
        .result
        .unwrap_or(settings);
    }
    let handle = match resident_model_for_slot(&settings, slot_id, Some(&model_path)) {
        Ok(handle) => handle,
        Err(blocked) => {
            return Ok(CommandResult::blocked(
                "mom_llama.model_slot_load",
                &blocked.readiness,
                blocked.blocker,
            ));
        }
    };
    let status = handle.status();
    let model_bytes = std::fs::metadata(&model_path)
        .map(|metadata| metadata.len())
        .unwrap_or_default();
    Ok(CommandResult::passed(
        "mom_llama.model_slot_load",
        "host_integrated",
        ModelSlot {
            slot_id,
            model_path: Some(model_path.display().to_string()),
            model_bytes,
            active_sequences: status.active_sequences,
            max_sequences: status.max_sequences,
            state: format!("{:?}", status.state).to_lowercase(),
            transport: "in_process".to_string(),
        },
        Vec::new(),
        Vec::new(),
        false,
        false,
    ))
}

pub fn model_slot_unload(slot_id: usize) -> Result<CommandResult<ModelSlot>> {
    unload_resident_slot(slot_id);
    Ok(CommandResult::passed(
        "mom_llama.model_slot_unload",
        "contracted",
        ModelSlot {
            slot_id,
            model_path: None,
            model_bytes: 0,
            active_sequences: 0,
            max_sequences: 0,
            state: "unloaded".to_string(),
            transport: "in_process".to_string(),
        },
        Vec::new(),
        Vec::new(),
        false,
        false,
    ))
}

fn config_from_settings(settings: &crate::config::Settings) -> ServerConfig {
    ServerConfig {
        model_path: settings.model_path.clone(),
        device: settings.native_device,
        context_tokens: settings.context_tokens,
        batch_tokens: settings.batch_tokens,
        max_sequences: settings.max_parallel_sequences,
        resident_memory_budget_bytes: settings.resident_memory_budget_bytes,
        transport: "in_process".to_string(),
    }
}
