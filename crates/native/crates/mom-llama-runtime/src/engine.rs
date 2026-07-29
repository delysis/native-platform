use crate::config::{Settings, resolve_settings};
use crate::native_runtime::resident_model;
use crate::receipts::{Blocker, CommandResult};
use anyhow::Result;
use llama_native_engine::LLAMA_CPP_BINDING_VERSION;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Copy, Default)]
pub struct EngineCheckOptions {
    pub fake_fixture: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EngineCheckOutput {
    pub runtime: String,
    pub model_path: String,
    pub help_check: String,
    pub prompt_smoke: String,
    pub transport: String,
    pub binding_version: String,
    pub backend: String,
}

pub fn engine_check(options: EngineCheckOptions) -> Result<CommandResult<EngineCheckOutput>> {
    let settings = resolve_settings()?;
    if options.fake_fixture {
        return Ok(CommandResult::passed(
            "mom_llama.engine_check",
            "fake_fixture_exercised",
            EngineCheckOutput {
                runtime: "fake_fixture".to_string(),
                model_path: settings
                    .model_path
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default(),
                help_check: "not_applicable".to_string(),
                prompt_smoke: "fixture_only".to_string(),
                transport: "fake_fixture".to_string(),
                binding_version: LLAMA_CPP_BINDING_VERSION.to_string(),
                backend: "fixture".to_string(),
            },
            Vec::new(),
            Vec::new(),
            false,
            true,
        ));
    }
    let handle = match resident_model(&settings) {
        Ok(handle) => handle,
        Err(blocked) => {
            return Ok(CommandResult::blocked(
                "mom_llama.engine_check",
                &blocked.readiness,
                blocked.blocker,
            ));
        }
    };
    let status = handle.status();
    let model_path = settings.model_path.as_ref().cloned().unwrap_or_default();
    let backend = status
        .fingerprint
        .as_ref()
        .map(|fingerprint| fingerprint.backend.clone())
        .unwrap_or_else(|| "native".to_string());
    Ok(CommandResult::passed(
        "mom_llama.engine_check",
        "host_integrated",
        EngineCheckOutput {
            runtime: "in_process_llama_cpp".to_string(),
            model_path: model_path.display().to_string(),
            help_check: "not_applicable".to_string(),
            prompt_smoke: "not_run".to_string(),
            transport: "in_process".to_string(),
            binding_version: LLAMA_CPP_BINDING_VERSION.to_string(),
            backend,
        },
        Vec::new(),
        Vec::new(),
        false,
        false,
    ))
}

#[derive(Debug, Clone)]
pub struct ValidationBlocker {
    pub readiness: String,
    pub blocker: Blocker,
}

pub fn validate_engine_and_model(
    settings: &Settings,
) -> std::result::Result<(), ValidationBlocker> {
    let Some(model) = settings.model_path.as_ref() else {
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
    validate_model_path(model)
}

pub fn validate_model_path(model: &Path) -> std::result::Result<(), ValidationBlocker> {
    if !model.exists() {
        return Err(ValidationBlocker {
            readiness: "blocked_missing_model".to_string(),
            blocker: Blocker::new(
                "model_path_missing",
                format!("Configured model path does not exist: {}.", model.display()),
                vec!["Choose an existing .gguf model file.".to_string()],
            ),
        });
    }
    if !model.is_file() {
        return Err(ValidationBlocker {
            readiness: "blocked_invalid_model".to_string(),
            blocker: Blocker::new(
                "model_path_not_file",
                format!("Configured model path is not a file: {}.", model.display()),
                vec!["Choose a .gguf model file.".to_string()],
            ),
        });
    }
    let is_gguf = model
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("gguf"));
    if !is_gguf {
        return Err(ValidationBlocker {
            readiness: "blocked_invalid_model".to_string(),
            blocker: Blocker::new(
                "model_path_not_gguf",
                format!(
                    "Configured model path is not a .gguf file: {}.",
                    model.display()
                ),
                vec!["Choose a .gguf model file.".to_string()],
            ),
        });
    }
    Ok(())
}
