use crate::config::{resolve_settings, save_settings};
use crate::engine::validate_model_path;
use crate::receipts::CommandResult;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

const MAX_DISCOVERED_MODELS: usize = 512;
const MAX_CACHE_SCAN_DEPTH: usize = 8;

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
    let mut seen = BTreeSet::new();
    if let Some(path) = settings.model_path.as_ref() {
        push_model(&mut models, &mut seen, path.clone(), true);
        if let Some(parent) = path.parent()
            && let Ok(entries) = fs::read_dir(parent)
        {
            for entry in entries.flatten() {
                let candidate = entry.path();
                if candidate == *path || !is_gguf(&candidate) {
                    continue;
                }
                push_model(&mut models, &mut seen, candidate, false);
            }
        }
    }
    if let Some(cache_dir) = hugging_face_hub_cache_dir() {
        let mut cached = Vec::new();
        collect_cached_models(&cache_dir, 0, &mut cached);
        cached.sort_by(|left, right| {
            model_file_name(left)
                .cmp(&model_file_name(right))
                .then_with(|| left.cmp(right))
        });
        for path in cached {
            let selected = settings.model_path.as_ref() == Some(&path);
            push_model(&mut models, &mut seen, path, selected);
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

pub fn hugging_face_hub_cache_dir() -> Option<PathBuf> {
    if let Some(path) =
        nonempty_env_path("HF_HUB_CACHE").or_else(|| nonempty_env_path("HUGGINGFACE_HUB_CACHE"))
    {
        return Some(path);
    }
    if let Some(path) = nonempty_env_path("HF_HOME") {
        return Some(path.join("hub"));
    }
    if let Some(path) = nonempty_env_path("XDG_CACHE_HOME") {
        return Some(path.join("huggingface").join("hub"));
    }
    user_home_dir().map(|home| home.join(".cache").join("huggingface").join("hub"))
}

fn nonempty_env_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn user_home_dir() -> Option<PathBuf> {
    nonempty_env_path("HOME")
        .or_else(|| nonempty_env_path("USERPROFILE"))
        .or_else(|| {
            let drive = std::env::var_os("HOMEDRIVE").filter(|value| !value.is_empty())?;
            let path = std::env::var_os("HOMEPATH").filter(|value| !value.is_empty())?;
            let mut home = drive;
            home.push(path);
            Some(PathBuf::from(home))
        })
}

fn collect_cached_models(directory: &Path, depth: usize, models: &mut Vec<PathBuf>) {
    if depth > MAX_CACHE_SCAN_DEPTH || models.len() >= MAX_DISCOVERED_MODELS {
        return;
    }
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        if models.len() >= MAX_DISCOVERED_MODELS {
            return;
        }
        let path = entry.path();
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };
        if metadata.is_dir() {
            collect_cached_models(&path, depth + 1, models);
        } else if metadata.is_file() && is_model_gguf(&path) {
            models.push(path);
        }
    }
}

fn push_model(
    models: &mut Vec<ModelInfo>,
    seen: &mut BTreeSet<PathBuf>,
    path: PathBuf,
    selected: bool,
) {
    let identity = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
    if !seen.insert(identity) {
        if selected
            && let Some(existing) = models
                .iter_mut()
                .find(|model| model.path == path.display().to_string())
        {
            existing.selected = true;
        }
        return;
    }
    models.push(ModelInfo {
        id: model_file_name(&path),
        path: path.display().to_string(),
        selected,
        size_bytes: fs::metadata(&path).ok().map(|metadata| metadata.len()),
    });
}

fn model_file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("model.gguf")
        .to_string()
}

fn is_model_gguf(path: &Path) -> bool {
    if !is_gguf(path) {
        return false;
    }
    let name = model_file_name(path).to_ascii_lowercase();
    !name.starts_with("mmproj-") && !name.contains("-mtp.")
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

#[cfg(test)]
mod tests {
    use super::hugging_face_hub_cache_dir;

    #[test]
    fn default_hugging_face_cache_uses_the_shared_desktop_location() {
        if std::env::var_os("HF_HUB_CACHE").is_none()
            && std::env::var_os("HUGGINGFACE_HUB_CACHE").is_none()
            && std::env::var_os("HF_HOME").is_none()
            && std::env::var_os("XDG_CACHE_HOME").is_none()
        {
            let cache = hugging_face_hub_cache_dir()
                .expect("the platform home directory should resolve a cache path");
            assert!(
                cache.ends_with(
                    std::path::Path::new(".cache")
                        .join("huggingface")
                        .join("hub")
                )
            );
        }
    }
}
