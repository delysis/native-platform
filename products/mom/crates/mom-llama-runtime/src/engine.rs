use crate::config::{Settings, resolve_settings};
use crate::native_runtime::{resident_model, resident_slots, resident_status};
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

pub fn engine_status() -> Result<CommandResult<EngineCheckOutput>> {
    let settings = resolve_settings()?;
    if let Err(blocked) = validate_engine_and_model(&settings) {
        return Ok(CommandResult::blocked(
            "mom_llama.engine_check",
            &blocked.readiness,
            blocked.blocker,
        ));
    }
    let model_path = settings.model_path.as_ref().cloned().unwrap_or_default();
    let slot = resident_slots().into_iter().find(|slot| slot.slot_id == 0);
    let resident = resident_status().filter(|status| {
        let Some(slot) = slot.as_ref() else {
            return false;
        };
        let Some(fingerprint) = status.fingerprint.as_ref() else {
            return false;
        };
        slot.model_path == model_path
            && fingerprint.context_tokens == settings.context_tokens
            && fingerprint.batch_tokens == settings.batch_tokens
            && fingerprint.max_sequences == settings.max_parallel_sequences.clamp(1, 4)
    });
    let (readiness, backend) = resident.and_then(|status| status.fingerprint).map_or_else(
        || ("configured", "not_loaded".to_string()),
        |fingerprint| ("host_integrated", fingerprint.backend),
    );
    Ok(CommandResult::passed(
        "mom_llama.engine_check",
        readiness,
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

impl std::fmt::Display for ValidationBlocker {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.blocker.code, self.blocker.message)
    }
}

impl std::error::Error for ValidationBlocker {}

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
    if model.as_os_str().is_empty() || model.to_str().is_some_and(|value| value.trim().is_empty()) {
        return Err(ValidationBlocker {
            readiness: "blocked_missing_model".to_string(),
            blocker: Blocker::new(
                "model_path_missing",
                "No GGUF model is configured.",
                vec!["Choose a local GGUF model.".to_string()],
            ),
        });
    }
    if !model.exists() {
        return Err(ValidationBlocker {
            readiness: "blocked_missing_model".to_string(),
            blocker: Blocker::new(
                "model_path_missing",
                "That model file is no longer available.",
                vec!["Choose an available GGUF model file.".to_string()],
            ),
        });
    }
    if !model.is_file() {
        return Err(ValidationBlocker {
            readiness: "blocked_invalid_model".to_string(),
            blocker: Blocker::new(
                "model_path_not_file",
                "Choose a GGUF model file, not a folder.",
                vec!["Choose a file ending in .gguf.".to_string()],
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
                "Choose a GGUF model file.",
                vec!["Choose a file ending in .gguf.".to_string()],
            ),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_model_path;
    use std::path::Path;

    #[test]
    fn model_path_blockers_are_friendly_and_never_echo_raw_paths() {
        let empty = validate_model_path(Path::new("")).expect_err("empty model path");
        assert_eq!(empty.blocker.message, "No GGUF model is configured.");
        assert!(!empty.blocker.message.contains(": ."));

        let missing_path = std::env::temp_dir().join(format!(
            "mom-llama-missing-private-model-{}.gguf",
            crate::now_ms()
        ));
        let missing = validate_model_path(&missing_path).expect_err("missing model path");
        assert_eq!(
            missing.blocker.message,
            "That model file is no longer available."
        );
        assert!(
            !missing
                .blocker
                .message
                .contains(&missing_path.display().to_string())
        );

        let directory =
            std::env::temp_dir().join(format!("mom-llama-invalid-model-kind-{}", crate::now_ms()));
        std::fs::create_dir_all(&directory).expect("model test directory");
        let not_file = validate_model_path(&directory).expect_err("directory is not a model");
        assert_eq!(
            not_file.blocker.message,
            "Choose a GGUF model file, not a folder."
        );
        let text = directory.join("private-name.txt");
        std::fs::write(&text, b"not a GGUF").expect("non-GGUF file");
        let not_gguf = validate_model_path(&text).expect_err("wrong model extension");
        assert_eq!(not_gguf.blocker.message, "Choose a GGUF model file.");
        assert!(!not_gguf.blocker.message.contains("private-name"));
        std::fs::remove_dir_all(directory).expect("remove model test directory");
    }
}
