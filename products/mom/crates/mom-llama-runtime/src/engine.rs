use crate::config::{
    Settings, VerifiedArtifactDigest, VerifiedModelArtifacts, persist_verified_model_artifacts,
    resolve_settings,
};
use crate::native_runtime::{resident_model, resident_slots, resident_status};
use crate::receipts::{Blocker, CommandResult};
use anyhow::Result;
use llama_native_engine::LLAMA_CPP_BINDING_VERSION;
use llama_native_types::{
    ChatMessage, ChatRole, ChatTemplateChoice, GenerationInput, GenerationRequest, MediaKind,
    NativeModelLoadMetrics, SamplingConfig,
};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeWarmOutput {
    pub model_loaded: bool,
    pub audio_projector_warmed: bool,
    pub audio_projector_warm_us: u64,
    pub persona_prefixes_ready: usize,
    pub persona_prefixes_reused: usize,
    pub settings_us: u64,
    pub model_acquire_us: u64,
    pub model_load: Option<NativeModelLoadMetrics>,
}

/// Loads the configured model as soon as the shell exists.
///
/// Persona prefixes are built on first use and retained in the resident memory
/// cache. Walking every Persona here serialized and encrypted hundreds of
/// megabytes per prefix for long-context hybrid models, turning startup into a
/// minute-long CPU job and delaying foreground calls behind background work.
pub fn runtime_warm(scope: &crate::OperationScope) -> Result<CommandResult<RuntimeWarmOutput>> {
    runtime_warm_until_cancelled(scope, None)
}

pub fn runtime_warm_until_cancelled(
    scope: &crate::OperationScope,
    cancellation: Option<&AtomicBool>,
) -> Result<CommandResult<RuntimeWarmOutput>> {
    if cancellation.is_some_and(|control| control.load(Ordering::Acquire)) {
        return Ok(runtime_warm_result(false, false, 0, 0, 0, 0, 0, None));
    }
    let settings_started = std::time::Instant::now();
    let settings = resolve_settings()?;
    let settings_us = settings_started
        .elapsed()
        .as_micros()
        .min(u128::from(u64::MAX)) as u64;
    let acquire_started = std::time::Instant::now();
    let handle = match resident_model(scope, &settings) {
        Ok(handle) => handle,
        Err(blocked) => {
            return Ok(CommandResult::blocked(
                "mom_llama.runtime_warm",
                &blocked.readiness,
                blocked.blocker,
            ));
        }
    };
    let model_acquire_us = acquire_started
        .elapsed()
        .as_micros()
        .min(u128::from(u64::MAX)) as u64;
    let status = handle.status();
    let model_load = status
        .descriptor
        .as_ref()
        .map(|descriptor| descriptor.load_metrics.clone());
    if let (Some(descriptor), Some(fingerprint), Some(model_path)) = (
        status.descriptor.as_ref(),
        status.fingerprint.as_ref(),
        settings.model_path.as_ref(),
    ) && !descriptor.model_file_identity.is_empty()
    {
        let projector = settings
            .mmproj_path
            .as_ref()
            .zip(fingerprint.multimodal_projector_sha256.as_ref())
            .zip(descriptor.mmproj_file_identity.as_ref())
            .map(|((path, sha256), file_identity)| VerifiedArtifactDigest {
                path: path.clone(),
                sha256: sha256.clone(),
                file_identity: file_identity.clone(),
            });
        persist_verified_model_artifacts(
            &settings,
            VerifiedModelArtifacts {
                model: VerifiedArtifactDigest {
                    path: model_path.clone(),
                    sha256: fingerprint.model_sha256.clone(),
                    file_identity: descriptor.model_file_identity.clone(),
                },
                projector,
            },
        )?;
    }
    let audio_warm_started = std::time::Instant::now();
    let supports_audio = status.descriptor.as_ref().is_some_and(|descriptor| {
        descriptor
            .capabilities
            .media_kinds
            .contains(&MediaKind::Audio)
    });
    let audio_projector_warmed = if supports_audio {
        warm_audio_projector(&handle, cancellation)?
    } else {
        false
    };
    let audio_projector_warm_us = if audio_projector_warmed {
        audio_warm_started
            .elapsed()
            .as_micros()
            .min(u128::from(u64::MAX)) as u64
    } else {
        0
    };
    drop(handle);
    Ok(runtime_warm_result(
        true,
        audio_projector_warmed,
        audio_projector_warm_us,
        0,
        0,
        settings_us,
        model_acquire_us,
        model_load,
    ))
}

fn runtime_warm_result(
    model_loaded: bool,
    audio_projector_warmed: bool,
    audio_projector_warm_us: u64,
    persona_prefixes_ready: usize,
    persona_prefixes_reused: usize,
    settings_us: u64,
    model_acquire_us: u64,
    model_load: Option<NativeModelLoadMetrics>,
) -> CommandResult<RuntimeWarmOutput> {
    CommandResult::passed(
        "mom_llama.runtime_warm",
        "host_integrated",
        RuntimeWarmOutput {
            model_loaded,
            audio_projector_warmed,
            audio_projector_warm_us,
            persona_prefixes_ready,
            persona_prefixes_reused,
            settings_us,
            model_acquire_us,
            model_load,
        },
        Vec::new(),
        Vec::new(),
        true,
        false,
    )
}

/// Compile the audio projector and its first recurrent-model evaluation before
/// the application advertises voice readiness. Loading GGUF weights alone does
/// not compile Metal pipelines; without this probe the caller's first 40 ms
/// frame can stall for hundreds of milliseconds after they stop speaking.
fn warm_audio_projector(
    handle: &llama_native_engine::NativeModelHandle,
    cancellation: Option<&AtomicBool>,
) -> Result<bool> {
    let mut sampling = SamplingConfig::default();
    sampling.max_tokens = 1;
    sampling.temperature = 0.0;
    let generation = handle.generate_incremental_audio(GenerationRequest {
        request_id: format!("runtime-audio-warm-{}", uuid::Uuid::new_v4()),
        model_id: handle.status().model_id,
        input: GenerationInput::Chat {
            messages: vec![ChatMessage {
                role: ChatRole::User,
                content: "Warm the live audio path.".to_string(),
            }],
            template: ChatTemplateChoice::PreferNonThinking,
        },
        sampling,
        media: Vec::new(),
        cached_prefix: None,
    })?;
    generation.wait_for_resume()?;
    let frame = (0..640)
        .map(|index| {
            let phase = index as f32 * std::f32::consts::TAU * 220.0 / 16_000.0;
            phase.sin() * 0.01
        })
        .collect::<Vec<_>>();
    generation.push_pcm(&frame)?;
    generation.seal_input()?;
    let monitor = generation.monitor();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while monitor.stats().projected_blocks == 0 {
        if cancellation.is_some_and(|control| control.load(Ordering::Acquire)) {
            generation.cancel();
            let _ = generation.finish().and_then(|ticket| ticket.wait());
            return Ok(false);
        }
        if std::time::Instant::now() >= deadline {
            generation.cancel();
            let _ = generation.finish().and_then(|ticket| ticket.wait());
            anyhow::bail!("the live audio projector did not warm before its bounded deadline");
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    generation.cancel();
    let _ = generation.finish().and_then(|ticket| ticket.wait());
    Ok(true)
}

pub fn engine_status(scope: &crate::OperationScope) -> Result<CommandResult<EngineCheckOutput>> {
    let settings = resolve_settings()?;
    if let Err(blocked) = validate_engine_and_model(&settings) {
        return Ok(CommandResult::blocked(
            "mom_llama.engine_check",
            &blocked.readiness,
            blocked.blocker,
        ));
    }
    let model_path = settings.model_path.as_ref().cloned().unwrap_or_default();
    let slot = resident_slots(scope)
        .into_iter()
        .find(|slot| slot.slot_id == 0);
    let resident = resident_status(scope).filter(|status| {
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

pub fn engine_check(
    scope: &crate::OperationScope,
    options: EngineCheckOptions,
) -> Result<CommandResult<EngineCheckOutput>> {
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
    let handle = match resident_model(scope, &settings) {
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
    use super::{runtime_warm_until_cancelled, validate_model_path};
    use std::path::Path;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn cancelled_background_warm_never_touches_the_model() {
        let cancellation = AtomicBool::new(true);
        let output =
            runtime_warm_until_cancelled(&crate::OperationScope::detached(), Some(&cancellation))
                .expect("cancelled warm should be a cheap successful no-op")
                .result
                .expect("cancelled warm output");
        assert!(!output.model_loaded);
        assert!(!output.audio_projector_warmed);
        assert_eq!(output.audio_projector_warm_us, 0);
        assert_eq!(output.persona_prefixes_ready, 0);
        assert_eq!(output.persona_prefixes_reused, 0);
    }

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
