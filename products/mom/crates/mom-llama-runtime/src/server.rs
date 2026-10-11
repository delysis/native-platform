use crate::config::{SettingsUpdate, resolve_settings, settings_update};
use crate::engine::validate_model_path;
use crate::native_runtime::{resident_model_for_slot, resident_slots, unload_resident_slot};
use crate::receipts::CommandResult;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelSlot {
    pub slot_id: usize,
    pub model_path: Option<String>,
    pub model_bytes: u64,
    pub reserved_bytes: u64,
    pub active_sequences: usize,
    pub max_sequences: u32,
    pub state: String,
    pub transport: String,
}

pub fn model_slot_list(scope: &crate::OperationScope) -> Result<CommandResult<Vec<ModelSlot>>> {
    let slots = resident_slots(scope)
        .into_iter()
        .map(|slot| ModelSlot {
            slot_id: slot.slot_id,
            model_path: Some(slot.model_path.display().to_string()),
            model_bytes: slot.model_bytes,
            reserved_bytes: slot.reserved_bytes,
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

pub fn model_slot_load(
    scope: &crate::OperationScope,
    slot_id: usize,
    model_path: PathBuf,
) -> Result<CommandResult<ModelSlot>> {
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
            model_path: Some(Some(model_path.clone())),
            ..SettingsUpdate::default()
        })?
        .result
        .unwrap_or(settings);
    }
    let handle = match resident_model_for_slot(scope, &settings, slot_id, Some(&model_path)) {
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
    let reserved_bytes = resident_slots(scope)
        .into_iter()
        .find(|slot| slot.slot_id == slot_id)
        .map(|slot| slot.reserved_bytes)
        .unwrap_or(model_bytes);
    Ok(CommandResult::passed(
        "mom_llama.model_slot_load",
        "host_integrated",
        ModelSlot {
            slot_id,
            model_path: Some(model_path.display().to_string()),
            model_bytes,
            reserved_bytes,
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

pub fn model_slot_unload(
    scope: &crate::OperationScope,
    slot_id: usize,
) -> Result<CommandResult<ModelSlot>> {
    unload_resident_slot(scope, slot_id);
    Ok(CommandResult::passed(
        "mom_llama.model_slot_unload",
        "contracted",
        ModelSlot {
            slot_id,
            model_path: None,
            model_bytes: 0,
            reserved_bytes: 0,
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
