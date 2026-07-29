use crate::config::{resolve_settings, save_settings};
use crate::engine::validate_model_path;
use crate::receipts::CommandResult;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelInfo {
    pub id: String,
    pub path: String,
    pub selected: bool,
    pub size_bytes: Option<u64>,
}

pub fn model_list() -> Result<CommandResult<Vec<ModelInfo>>> {
    let settings = resolve_settings()?;
    let mut models = Vec::new();
    if let Some(path) = settings.model_path.as_ref() {
        models.push(ModelInfo {
            id: path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("selected.gguf")
                .to_string(),
            path: path.display().to_string(),
            selected: true,
            size_bytes: fs::metadata(path).ok().map(|metadata| metadata.len()),
        });
        if let Some(parent) = path.parent()
            && let Ok(entries) = fs::read_dir(parent)
        {
            for entry in entries.flatten() {
                let candidate = entry.path();
                if candidate == *path || !is_gguf(&candidate) {
                    continue;
                }
                models.push(ModelInfo {
                    id: candidate
                        .file_name()
                        .and_then(|value| value.to_str())
                        .unwrap_or("model.gguf")
                        .to_string(),
                    path: candidate.display().to_string(),
                    selected: false,
                    size_bytes: fs::metadata(&candidate).ok().map(|metadata| metadata.len()),
                });
            }
        }
    }
    Ok(CommandResult::passed(
        "mom_llama.model_list",
        "contracted",
        models,
        Vec::new(),
        Vec::new(),
        false,
        false,
    ))
}

pub fn model_select(model_path: PathBuf) -> Result<CommandResult<crate::Settings>> {
    if let Err(blocked) = validate_model_path(&model_path) {
        return Ok(CommandResult::blocked(
            "mom_llama.model_select",
            &blocked.readiness,
            blocked.blocker,
        ));
    }
    let mut settings = resolve_settings()?;
    settings.model_path = Some(model_path);
    let path = save_settings(&settings)?;
    Ok(CommandResult::passed(
        "mom_llama.model_select",
        "contracted",
        settings,
        vec![path.display().to_string()],
        Vec::new(),
        false,
        false,
    ))
}

fn is_gguf(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("gguf"))
}
